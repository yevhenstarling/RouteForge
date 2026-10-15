#!/usr/bin/env sh
# Shows what RouteForge claims about its workers, on the stack as Docker Compose runs it.
#
#   scaling   The same day is planned with 1 solver and then with 4. The routes are the
#             same, and they are there sooner.
#   failure   A solver is killed in the middle of a route. When its lease has run out,
#             another solver takes the route over, and every order is routed once.
#
#   scripts/demo.sh                               both, one after the other
#   DEMO_SOLVERS='1 2 4 8' scripts/demo.sh scaling
#   DEMO_BUDGET_SECS=15 scripts/demo.sh failure
#
# Needs Docker, curl and jq. It brings the stack up and builds what is missing, logs in
# as the admin the stack was started with, and makes what a day needs if it is not
# there: a depot, six vans, and orders.
#
# The day it plans is tomorrow unless DEMO_DAY says otherwise, so that it stays out of
# the way of whoever is at the page, which opens on today. On its day it cancels a plan
# that is in progress and discards one that is done (D30). That gives their orders back
# and loses nothing else. It leaves the stack up, and the last plan done, for the page
# to show.
#
# A killed solver's route is taken over when its lease has run out, which is 30 seconds
# by default. For a quicker show, set SOLVER_LEASE_SECS and SOLVER_HEARTBEAT_SECS in the
# shell first, to 10 and 2 for example. Compose passes them on to the solvers.
set -eu
# Numbers with a point in them, whatever this machine's language is.
export LC_ALL=C

run=${1:-all}
case $run in
    all | scaling | failure) ;;
    *)
        echo "usage: $0 [all|scaling|failure]" >&2
        exit 2
        ;;
esac
for tool in docker curl jq; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "$0 needs $tool" >&2
        exit 1
    fi
done

cd "$(dirname "$0")/.."

# The day that is planned.
day=${DEMO_DAY:-$(jq -rn 'now + 86400 | strflocaltime("%Y-%m-%d")')}
# How many orders the day gets if it has none. 0: as many as the couriers carry.
orders=${DEMO_ORDERS:-0}
# What the orders are scattered by, and what every search draws its choices from.
seed=${DEMO_SEED:-7}
# Scaling: the budget of each route, in millions of candidate moves.
moves=${DEMO_MOVES:-200}
# Scaling: how many solvers each run has. The failure run has the most of them.
solvers=${DEMO_SOLVERS:-1 4}
# Failure: the budget of each route, in seconds.
budget_secs=${DEMO_BUDGET_SECS:-10}

# A list with commas in it is taken as well. A solver holds four connections to the
# database and PostgreSQL gives out a hundred, which makes twenty solvers the most this
# stack runs. Anything else is refused before compose is asked for it.
solvers=$(printf '%s' "$solvers" | tr ',' ' ')
most=0
for count in $solvers; do
    case $count in
        *[!0-9]* | ???*) count=0 ;;
    esac
    if [ "$count" -lt 1 ] || [ "$count" -gt 20 ]; then
        echo "DEMO_SOLVERS takes numbers from 1 to 20, as in '1 4'. It is '$solvers'." >&2
        exit 2
    fi
    if [ "$count" -gt "$most" ]; then most=$count; fi
done
if [ "$most" -lt 1 ]; then
    echo "DEMO_SOLVERS takes numbers from 1 to 20, as in '1 4'. It is empty." >&2
    exit 2
fi
case $budget_secs in
    '' | *[!0-9]* | ????*) budget_secs=0 ;;
esac
if [ "$budget_secs" -lt 5 ] || [ "$budget_secs" -gt 300 ]; then
    # The solver is killed two seconds in, which has to be the middle of a search.
    echo 'DEMO_BUDGET_SECS takes 5 to 300 seconds.' >&2
    exit 2
fi

stamp=$(date +%s)
request_id=demo-$stamp

# json TEXT FILTER...: what jq makes of the text.
json() {
    text=$1
    shift
    printf '%s' "$text" | jq -r "$@"
}

# api METHOD PATH [BODY]: what the API answers, asked as whoever is logged in. A refusal
# is a failure, and says the reason the API gave.
api() {
    method=$1
    path=$2
    shift 2
    if [ $# -gt 0 ]; then
        set -- --header 'Content-Type: application/json' --data "$1"
    fi
    if ! answer=$(curl --silent --show-error --fail-with-body --request "$method" "$base$path" \
        --header "Authorization: Bearer $token" --header "X-Request-Id: $request_id" "$@"); then
        echo "$method $path was refused: $answer" >&2
        return 1
    fi
    printf '%s\n' "$answer"
}

# wait_for WHAT SECONDS COMMAND...: runs the command twice a second until it succeeds.
wait_for() {
    what=$1
    deadline=$(($(date +%s) + $2))
    shift 2
    until "$@"; do
        if [ "$(date +%s)" -ge "$deadline" ]; then
            echo "Waited too long for $what." >&2
            exit 1
        fi
        sleep 0.5
    done
}

# Whether the API sees exactly that many solvers, with nothing to do.
idle() {
    stats=$(api GET /api/system/stats) &&
        [ "$(json "$stats" --argjson count "$1" '.workers_alive == $count and .workers_busy == 0')" = true ]
}

# Runs that many solvers, and waits until the API sees exactly them, idle. A solver
# that was stopped counts as alive for a few seconds more.
scale() {
    docker compose up -d --wait --scale "solver=$1"
    wait_for "$1 solvers to be alive and idle" 90 idle "$1"
}

# Gives the day its orders back: they are pending again, whatever plan had them.
clear_day() {
    plans=$(api GET "/api/plans?service_date=$day")
    for id in $(json "$plans" '.[] | select(.status == "queued" or .status == "routing") | .id'); do
        api POST "/api/plans/$id/cancel" >/dev/null
    done
    for id in $(json "$plans" '.[] | select(.status == "done") | .id'); do
        api POST "/api/plans/$id/discard" >/dev/null
    done
}

# asked BUDGET: asks for a plan of the day with this budget for each of its routes, and
# leaves it in $plan.
asked() {
    plan=$(api POST /api/plans "$(jq -n --arg day "$day" --argjson seed "$seed" --argjson budget "$1" \
        '{service_date: $day, seed: $seed, budget: $budget}')")
    plan_id=$(json "$plan" .id)
}

# Whether the plan has ended, one way or another. Leaves it in $plan.
ended() {
    plan=$(api GET "/api/plans/$plan_id") &&
        [ "$(json "$plan" '.status != "queued" and .status != "routing"')" = true ]
}

is_done() {
    if [ "$(json "$plan" .status)" != done ]; then
        echo "The plan is $(json "$plan" .status): $(json "$plan" '.error // ""')" >&2
        exit 1
    fi
}

# How long the plan took, from being asked for to being done, by the database's clock.
took() {
    json "$plan" '
        def at: (.[0:19] + "Z" | fromdateiso8601)
            + (if test("\\.[0-9]+") then capture("(?<part>\\.[0-9]+)").part | tonumber else 0 end);
        (.finished_at | at) - (.created_at | at)'
}

# Counts the routes of the plan, which is done, after checking what the system is
# judged by: every order the plan took is on exactly one route. Leaves the counts in
# $route_count, $stop_count and $km, and in $same what two plans are compared by:
# whose route is how long and calls where, in which order.
routes() {
    stored=$(api GET "/api/plans/$plan_id/routes")
    counted=$(json "$stored" --argjson plan "$plan" '
        [.[].stops[].order_id] as $stops
        | ($plan.orders_total - $plan.orders_unassigned) as $taken
        | if length != $plan.routes_total or ($stops | length) != $taken or ($stops | unique | length) != $taken
          then error("The plan took \($taken) orders into \($plan.routes_total) routes, and \(length) routes hold \($stops | length) stops for \($stops | unique | length) orders.")
          else "\(length) \($stops | length) \((map(.distance_m) | add // 0) / 1000)"
          end')
    set -- $counted
    route_count=$1
    stop_count=$2
    km=$3
    same=$(json "$stored" -c 'sort_by(.courier_id) | map([.courier_id, .distance_m, [.stops[].order_id]])')
}

scaling() {
    echo
    echo "Scaling: the orders of $day planned with $(echo "$solvers" | sed 's/ /, then /g') solvers. Every route is searched for $moves million moves, with seed $seed."
    columns='%7s  %7s  %8s  %6s  %5s  %8s  %s\n'
    rows=''
    first_took=''
    first_same=''
    differ=''
    for count in $solvers; do
        scale "$count"
        clear_day
        request_id=demo-scaling-$count-$stamp
        asked "$(jq -n --argjson moves "$moves" '{iterations: ($moves * 1000000)}')"
        echo "  Solvers: $count. Plan $plan_id, request id $request_id"
        wait_for 'the plan to end' 1800 ended
        is_done
        routes
        seconds=$(took)
        matches=-
        if [ -z "$first_took" ]; then
            first_took=$seconds
            first_same=$same
        elif [ "$same" = "$first_same" ]; then
            matches=yes
        else
            matches=NO
            differ=yes
        fi
        speed_up=$(jq -n --argjson first "$first_took" --argjson now "$seconds" '$first / $now')
        rows=$rows$(printf "$columns" "$count" "$(printf '%.1f' "$seconds")" "$(printf '%.1fx' "$speed_up")" \
            "$route_count" "$stop_count" "$(printf '%.2f' "$km")" "$matches")'
'
        if [ "$count" -gt "$route_count" ]; then
            echo "  The day has $route_count routes, so $count solvers cannot all have one."
        fi
    done
    echo
    printf "$columns" solvers seconds speed-up routes stops km 'same routes'
    printf '%s' "$rows"
    if [ -n "$differ" ]; then
        echo 'The same seed and the same budget of moves gave other routes (D5).' >&2
        exit 1
    fi
}

# Whether the plan has been split into routes.
routing() {
    plan=$(api GET "/api/plans/$plan_id") && [ "$(json "$plan" .status)" = routing ]
}

# Finds a solver of this stack that holds a job, and leaves its worker, its host name
# and the job in $victim. A worker's host name is the id of its container, as short as
# Docker shows it.
searching() {
    stats=$(api GET /api/system/stats) || return 1
    victim=$(json "$stats" --arg containers "$containers" '
        ($containers | split("\n")) as $ids
        | first(.workers[]
            | select(.current_job != null)
            | select(.hostname as $host | any($ids[]; startswith($host)))
            | "\(.id) \(.hostname) \(.current_job)")')
    [ -n "$victim" ]
}

# Whether another worker holds the job the killed one held. Leaves its host name in $heir.
taken_over() {
    stats=$(api GET /api/system/stats) || return 1
    heir=$(json "$stats" --arg worker "$victim_id" --arg job "$victim_job" \
        'first(.workers[] | select(.id != $worker and .current_job == $job) | .hostname)')
    [ -n "$heir" ]
}

failure() {
    if [ "$most" -lt 2 ]; then
        echo 'The failure run needs two solvers at least: one of them has to take the route over.' >&2
        exit 1
    fi
    echo
    echo "Failure: one of $most solvers is killed in the middle of a route. Every route is searched for $budget_secs seconds."
    scale "$most"
    clear_day
    request_id=demo-failure-$stamp
    asked "$(jq -n --argjson secs "$budget_secs" '{time_ms: ($secs * 1000)}')"
    echo "  Plan $plan_id, request id $request_id"
    wait_for 'the plan to be split into routes' 60 routing
    # Into the middle of the searches.
    sleep 2

    containers=$(docker compose ps -q solver)
    wait_for 'a solver in the middle of a route' 30 searching
    set -- $victim
    victim_id=$1
    victim_host=$2
    victim_job=$3
    docker kill "$victim_host" >/dev/null
    killed=$(jq -n now)
    echo "  Killed solver $victim_host, which was searching for the route of job $victim_job."

    heir=''
    give_up=$(($(date +%s) + budget_secs * 2 + 180))
    until ended; do
        if [ -z "$heir" ] && taken_over; then
            after=$(jq -n --argjson killed "$killed" 'now - $killed')
            echo "  Taken over by solver $heir, $(printf '%.1f' "$after") seconds after the kill."
        fi
        if [ "$(date +%s)" -ge "$give_up" ]; then
            echo 'The plan did not end. Is there a solver left to take the route over?' >&2
            exit 1
        fi
        sleep 0.5
    done
    is_done
    routes
    if [ -z "$heir" ]; then echo '  The route was taken over between two looks at the workers.'; fi
    echo "  Done $(printf '%.1f' "$(took)") seconds after it was asked for: $route_count routes, $stop_count stops, $(printf '%.2f' "$km") km, and every order on exactly one route."

    # The killed solver is started again.
    scale "$most"
}

start_with=${solvers%% *}
if [ "$run" = failure ]; then start_with=$most; fi
# Without --build: compose builds an image that is missing, and one that is there needs
# no registry to be asked, so the show goes on without a network.
echo 'Bringing the stack up. If the images have to be built, that takes several minutes.'
docker compose up -d --wait --scale "solver=$start_with"

# Where compose has published the API, and who it made an admin of when it started.
published=$(docker compose port api 8080)
base=http://$(printf '%s\n' "$published" | head -n 1 | sed 's/^0\.0\.0\.0/127.0.0.1/')
email=$(docker compose exec -T api printenv ADMIN_EMAIL)
password=$(docker compose exec -T api printenv ADMIN_PASSWORD)
login=$(jq -n --arg email "$email" --arg password "$password" '{email: $email, password: $password}')
if ! session=$(curl --silent --show-error --fail-with-body "$base/api/auth/login" \
    --header 'Content-Type: application/json' --data "$login"); then
    echo "Could not log in as $email: $session" >&2
    exit 1
fi
token=$(json "$session" .token)
echo "The API is at $base, and $email is logged in."

depot=$(curl --silent --output /dev/null --write-out '%{http_code}' \
    --header "Authorization: Bearer $token" "$base/api/depot")
if [ "$depot" = 404 ]; then
    api PUT /api/depot '{"name": "Depot", "location": {"lat": 50.45, "lon": 30.52}}' >/dev/null
    echo 'Set a depot, in Kyiv.'
fi
couriers=$(api GET /api/couriers)
if [ "$(json "$couriers" length)" = 0 ]; then
    # As the page sets up its demo city. A demo order has a demand of 1 to 3, and four in
    # five are deliveries, so a day sends 1.6 times its number out of the depot. That is
    # what a van needs room for: a pickup comes on board when deliveries are off (D65).
    # Six vans take it with a tenth to spare (D88).
    sized_for=1200
    if [ "$orders" -gt 0 ]; then sized_for=$orders; fi
    capacity=$(jq -n --argjson orders "$sized_for" '$orders * 2 * 0.8 * 1.1 / 6 | ceil')
    for van in 1 2 3 4 5 6; do
        api POST /api/couriers "$(jq -n --arg name "Van $van" --argjson capacity "$capacity" \
            '{name: $name, capacity: $capacity}')" >/dev/null
    done
    couriers=$(api GET /api/couriers)
    echo "Added six vans, each for $capacity."
fi
existing=$(api GET "/api/orders?service_date=$day&limit=1")
if [ "$(json "$existing" length)" = 0 ]; then
    count=$orders
    if [ "$count" -le 0 ]; then
        # That sum turned round: as many orders as the couriers carry with a tenth to
        # spare. Then every courier gets a route, and every solver has one to search for.
        count=$(json "$couriers" '[.[] | select(.active) | .capacity] | add // 0 | . / 1.76 | floor
            | if . < 1 then 1 elif . > 2000 then 2000 else . end')
    fi
    made=$(api POST /api/demo/orders "$(jq -n --arg day "$day" --argjson count "$count" --argjson seed "$seed" \
        '{service_date: $day, count: $count, seed: $seed}')")
    echo "Generated $(json "$made" .created) orders for $day, with seed $seed."
else
    echo "$day has orders already. They are planned as they are."
fi

if [ "$run" != failure ]; then scaling; fi
if [ "$run" != scaling ]; then failure; fi

echo
echo "The day is $day. Pick it on the page at $base/ and press Refresh to see its routes."
echo 'What the request for a plan left in the logs of the API and of the solvers:'
echo '  docker compose logs api solver | grep <request id>'
