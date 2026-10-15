# Shows what RouteForge claims about its workers, on the stack as Docker Compose runs it.
#
#   scaling   The same day is planned with 1 solver and then with 4. The routes are the
#             same, and they are there sooner.
#   failure   A solver is killed in the middle of a route. When its lease has run out,
#             another solver takes the route over, and every order is routed once.
#
#   scripts/demo.ps1                              both, one after the other
#   scripts/demo.ps1 scaling -Solvers 1,2,4,8
#   scripts/demo.ps1 failure -BudgetSecs 15
#
# Needs Docker, and nothing else. It brings the stack up and builds what is missing,
# logs in as the admin the stack was started with, and makes what a day needs if it is
# not there: a depot, six vans, and orders.
#
# The day it plans is tomorrow unless -Day says otherwise, so that it stays out of the
# way of whoever is at the page, which opens on today. On its day it cancels a plan
# that is in progress and discards one that is done (D30). That gives their orders back
# and loses nothing else. It leaves the stack up, and the last plan done, for the page
# to show.
#
# A killed solver's route is taken over when its lease has run out, which is 30 seconds
# by default. For a quicker show, set SOLVER_LEASE_SECS and SOLVER_HEARTBEAT_SECS in the
# shell first, to 10 and 2 for example. Compose passes them on to the solvers.
param(
    [ValidateSet('all', 'scaling', 'failure')]
    [string]$Run = 'all',
    # The day that is planned.
    [string]$Day = (Get-Date).AddDays(1).ToString('yyyy-MM-dd'),
    # How many orders the day gets if it has none. 0: as many as the couriers carry.
    [int]$Orders = 0,
    # What the orders are scattered by, and what every search draws its choices from.
    [uint32]$Seed = 7,
    # Scaling: the budget of each route, in millions of candidate moves.
    [ValidateRange(1, 10000)]
    [int]$Moves = 200,
    # Scaling: how many solvers each run has. The failure run has the most of them.
    [string[]]$Solvers = @('1', '4'),
    # Failure: the budget of each route, in seconds. The solver is killed two seconds in.
    [ValidateRange(5, 300)]
    [int]$BudgetSecs = 10
)

$ErrorActionPreference = 'Stop'
# Windows PowerShell draws a progress bar for every request, and is slow at it.
$ProgressPreference = 'SilentlyContinue'

# The counts are a list when they are typed in PowerShell, and the one text `1,2,4,8`
# when the script is started from anywhere else. Read as a number that text is 1248,
# and compose would start that many solvers. So they are taken as text and read here.
# A solver holds four connections to the database and PostgreSQL gives out a hundred,
# which makes twenty solvers the most this stack runs.
$Counts = @($Solvers | ForEach-Object { $_ -split '[,\s]+' } | Where-Object { $_ -ne '' })
foreach ($count in $Counts) {
    if ($count -notmatch '^\d{1,2}$' -or [int]$count -lt 1 -or [int]$count -gt 20) {
        throw "-Solvers takes numbers from 1 to 20, as in 1,4. It was given '$count'."
    }
}
if ($Counts.Count -eq 0) { throw '-Solvers takes numbers from 1 to 20, as in 1,4. It was given none.' }
$Counts = @($Counts | ForEach-Object { [int]$_ })
$Most = ($Counts | Measure-Object -Maximum).Maximum

function Compose {
    docker compose @args
    if ($LASTEXITCODE -ne 0) { throw "docker compose $args failed" }
}

# Asks the API, as whoever is logged in. A refusal ends the script with the reason the
# API gave.
function Api([string]$Method, [string]$Path, $Body, [string]$RequestId) {
    $call = @{ Method = $Method; Uri = "$Base$Path"; Headers = @{}; UseBasicParsing = $true }
    if ($Token) { $call.Headers['Authorization'] = "Bearer $Token" }
    if ($RequestId) { $call.Headers['X-Request-Id'] = $RequestId }
    if ($null -ne $Body) {
        $call['ContentType'] = 'application/json'
        $call['Body'] = ConvertTo-Json $Body -Compress
    }
    try {
        # Windows PowerShell hands a JSON array on as one thing. Passed through the
        # pipeline once more it comes apart, as it does by itself in PowerShell 7.
        Invoke-RestMethod @call | ForEach-Object { $_ }
    } catch {
        $script:Refused = 0
        if ($_.Exception.Response) { $script:Refused = [int]$_.Exception.Response.StatusCode }
        throw "$Method $Path was refused ($script:Refused): $($_.ErrorDetails.Message)"
    }
}

# Whether the API has the thing at this path. It answers 404 for one it has not.
function Exists([string]$Path) {
    try {
        Api GET $Path | Out-Null
        return $true
    } catch {
        if ($script:Refused -eq 404) { return $false }
        throw
    }
}

# A number as the other script prints it, whatever this machine's language is.
function Num([double]$Value, [string]$Format) {
    $Value.ToString($Format, [cultureinfo]::InvariantCulture)
}

# Runs $Until a few times a second until it gives something, and gives that.
function WaitFor([string]$What, [int]$Seconds, [scriptblock]$Until) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ($true) {
        $got = & $Until
        if ($got) { return $got }
        if ((Get-Date) -gt $deadline) { throw "Waited $Seconds seconds for $What." }
        Start-Sleep -Milliseconds 250
    }
}

# Runs that many solvers, and waits until the API sees exactly them, with nothing to do.
# A solver that was stopped counts as alive for a few seconds more.
function Scale([int]$Count) {
    Compose up -d --wait --scale "solver=$Count" | Out-Host
    WaitFor "$Count solvers to be alive and idle" 90 {
        $stats = Api GET '/api/system/stats'
        $stats.workers_alive -eq $Count -and $stats.workers_busy -eq 0
    } | Out-Null
}

# Gives the day its orders back: they are pending again, whatever plan had them.
function ClearDay {
    foreach ($plan in @(Api GET "/api/plans?service_date=$Day")) {
        if ($plan.status -eq 'queued' -or $plan.status -eq 'routing') {
            Api POST "/api/plans/$($plan.id)/cancel" | Out-Null
        } elseif ($plan.status -eq 'done') {
            Api POST "/api/plans/$($plan.id)/discard" | Out-Null
        }
    }
}

# Asks for a plan of the day with this budget for each of its routes.
function Asked($Budget, [string]$RequestId) {
    Api POST '/api/plans' @{ service_date = $Day; seed = $Seed; budget = $Budget } $RequestId
}

function Ended($Plan) {
    $Plan.status -ne 'queued' -and $Plan.status -ne 'routing'
}

# Waits for a plan to end, and gives it. One that ends as anything but done ends the
# script.
function Finished([string]$PlanId, [int]$Seconds) {
    $plan = WaitFor 'the plan to end' $Seconds {
        $now = Api GET "/api/plans/$PlanId"
        if (Ended $now) { $now }
    }
    if ($plan.status -ne 'done') { throw "The plan is $($plan.status): $($plan.error)" }
    $plan
}

# How long a plan took, from being asked for to being done, by the database's clock.
function Took($Plan) {
    ([datetime]$Plan.finished_at - [datetime]$Plan.created_at).TotalSeconds
}

# The routes of a plan that is done, counted, after checking what the system is judged
# by: every order the plan took is on exactly one route.
function Routes($Plan) {
    $routes = @(Api GET "/api/plans/$($Plan.id)/routes" | Sort-Object courier_id)
    $stops = @($routes | ForEach-Object { $_.stops } | ForEach-Object { $_.order_id })
    $distinct = @($stops | Sort-Object -Unique)
    $taken = $Plan.orders_total - $Plan.orders_unassigned
    if ($routes.Count -ne $Plan.routes_total -or $stops.Count -ne $taken -or $distinct.Count -ne $taken) {
        throw "The plan took $taken orders into $($Plan.routes_total) routes, and $($routes.Count) routes hold $($stops.Count) stops for $($distinct.Count) orders."
    }
    $metres = ($routes | Measure-Object distance_m -Sum).Sum
    [pscustomobject]@{
        Routes = $routes.Count
        Stops  = $stops.Count
        Km     = $metres / 1000
        # Whose route is how long and calls where, in which order: what two plans are
        # compared by.
        Same   = ($routes | ForEach-Object {
                "$($_.courier_id) $($_.distance_m) " + (($_.stops | ForEach-Object { $_.order_id }) -join ',')
            }) -join "`n"
    }
}

function ScalingRun {
    Write-Host ''
    Write-Host "Scaling: the orders of $Day planned with $($Counts -join ', then ') solvers. Every route is searched for $Moves million moves, with seed $Seed."
    $columns = '{0,7}  {1,7}  {2,8}  {3,6}  {4,5}  {5,8}  {6}'
    $rows = @()
    $first = $null
    foreach ($count in $Counts) {
        Scale $count
        ClearDay
        $requestId = "demo-scaling-$count-$Stamp"
        $plan = Asked @{ iterations = [long]$Moves * 1000000 } $requestId
        Write-Host "  Solvers: $count. Plan $($plan.id), request id $requestId"
        $plan = Finished $plan.id 1800
        $routes = Routes $plan
        $seconds = Took $plan
        $same = '-'
        if ($null -eq $first) {
            $first = @{ Seconds = $seconds; Same = $routes.Same }
        } elseif ($routes.Same -eq $first.Same) {
            $same = 'yes'
        } else {
            $same = 'NO'
        }
        $speedUp = (Num ($first.Seconds / $seconds) '0.0') + 'x'
        $rows += $columns -f $count, (Num $seconds '0.0'), $speedUp, $routes.Routes, $routes.Stops, (Num $routes.Km '0.00'), $same
        if ($count -gt $routes.Routes) {
            Write-Host "  The day has $($routes.Routes) routes, so $count solvers cannot all have one."
        }
    }
    Write-Host ''
    Write-Host ($columns -f 'solvers', 'seconds', 'speed-up', 'routes', 'stops', 'km', 'same routes')
    $rows | ForEach-Object { Write-Host $_ }
    if ($rows -match 'NO$') {
        throw 'The same seed and the same budget of moves gave other routes (D5).'
    }
}

function FailureRun {
    $count = $Most
    if ($count -lt 2) {
        throw 'The failure run needs two solvers at least: one of them has to take the route over.'
    }
    Write-Host ''
    Write-Host "Failure: one of $count solvers is killed in the middle of a route. Every route is searched for $BudgetSecs seconds."
    Scale $count
    ClearDay
    $requestId = "demo-failure-$Stamp"
    $planId = (Asked @{ time_ms = $BudgetSecs * 1000 } $requestId).id
    Write-Host "  Plan $planId, request id $requestId"
    WaitFor 'the plan to be split into routes' 60 {
        (Api GET "/api/plans/$planId").status -eq 'routing'
    } | Out-Null
    # Into the middle of the searches.
    Start-Sleep -Seconds 2

    $containers = @(Compose ps -q solver)
    $victim = WaitFor 'a solver in the middle of a route' 30 {
        foreach ($worker in @((Api GET '/api/system/stats').workers)) {
            # A worker's host name is the id of its container, as short as Docker shows it.
            if ($worker.current_job -and @($containers -like "$($worker.hostname)*").Count -gt 0) {
                return $worker
            }
        }
    }
    docker kill $victim.hostname | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Could not kill solver $($victim.hostname)." }
    $killed = Get-Date
    Write-Host "  Killed solver $($victim.hostname), which was searching for the route of job $($victim.current_job)."

    $heir = $null
    $giveUp = $killed.AddSeconds($BudgetSecs * 2 + 180)
    while ($true) {
        $plan = Api GET "/api/plans/$planId"
        if (Ended $plan) { break }
        if ($null -eq $heir) {
            $heir = @((Api GET '/api/system/stats').workers) |
                Where-Object { $_.id -ne $victim.id -and $_.current_job -eq $victim.current_job } |
                Select-Object -First 1
            if ($heir) {
                $after = Num ((Get-Date) - $killed).TotalSeconds '0.0'
                Write-Host "  Taken over by solver $($heir.hostname), $after seconds after the kill."
            }
        }
        if ((Get-Date) -gt $giveUp) { throw 'The plan did not end. Is there a solver left to take the route over?' }
        Start-Sleep -Milliseconds 500
    }
    if ($plan.status -ne 'done') { throw "The plan is $($plan.status): $($plan.error)" }
    $routes = Routes $plan
    if ($null -eq $heir) { Write-Host '  The route was taken over between two looks at the workers.' }
    Write-Host "  Done $(Num (Took $plan) '0.0') seconds after it was asked for: $($routes.Routes) routes, $($routes.Stops) stops, $(Num $routes.Km '0.00') km, and every order on exactly one route."

    # The killed solver is started again.
    Scale $count
}

Push-Location (Join-Path $PSScriptRoot '..')
try {
    $Stamp = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    $startWith = $Counts[0]
    if ($Run -eq 'failure') { $startWith = $Most }
    # Without --build: compose builds an image that is missing, and one that is there
    # needs no registry to be asked, so the show goes on without a network.
    Write-Host 'Bringing the stack up. If the images have to be built, that takes several minutes.'
    Compose up -d --wait --scale "solver=$startWith" | Out-Host

    # Where compose has published the API, and who it made an admin of when it started.
    $Base = 'http://' + (@(Compose port api 8080)[0] -replace '^0\.0\.0\.0', '127.0.0.1')
    $Token = $null
    $email = Compose exec -T api printenv ADMIN_EMAIL
    $password = Compose exec -T api printenv ADMIN_PASSWORD
    $Token = (Api POST '/api/auth/login' @{ email = $email; password = $password }).token
    Write-Host "The API is at $Base, and $email is logged in."

    if (-not (Exists '/api/depot')) {
        Api PUT '/api/depot' @{ name = 'Depot'; location = @{ lat = 50.45; lon = 30.52 } } | Out-Null
        Write-Host 'Set a depot, in Kyiv.'
    }
    $couriers = @(Api GET '/api/couriers')
    if ($couriers.Count -eq 0) {
        # As the page sets up its demo city. A demo order has a demand of 1 to 3, and
        # four in five are deliveries, so a day sends 1.6 times its number out of the
        # depot. That is what a van needs room for: a pickup comes on board when
        # deliveries are off (D65). Six vans take it with a tenth to spare (D88).
        $sizedFor = 1200
        if ($Orders -gt 0) { $sizedFor = $Orders }
        $capacity = [int][math]::Ceiling($sizedFor * 2 * 0.8 * 1.1 / 6)
        foreach ($van in 1..6) {
            Api POST '/api/couriers' @{ name = "Van $van"; capacity = $capacity } | Out-Null
        }
        $couriers = @(Api GET '/api/couriers')
        Write-Host "Added six vans, each for $capacity."
    }
    if (@(Api GET "/api/orders?service_date=$Day&limit=1").Count -eq 0) {
        $count = $Orders
        if ($count -le 0) {
            # That sum turned round: as many orders as the couriers carry with a tenth to
            # spare. Then every courier gets a route, and every solver has one to search for.
            $carried = ($couriers | Where-Object { $_.active } | Measure-Object capacity -Sum).Sum
            $count = [int][math]::Max(1, [math]::Min(2000, [math]::Floor($carried / 1.76)))
        }
        $made = Api POST '/api/demo/orders' @{ service_date = $Day; count = $count; seed = $Seed }
        Write-Host "Generated $($made.created) orders for $Day, with seed $Seed."
    } else {
        Write-Host "$Day has orders already. They are planned as they are."
    }

    if ($Run -ne 'failure') { ScalingRun }
    if ($Run -ne 'scaling') { FailureRun }

    Write-Host ''
    Write-Host "The day is $Day. Pick it on the page at $Base/ and press Refresh to see its routes."
    Write-Host 'What the request for a plan left in the logs of the API and of the solvers:'
    Write-Host '  docker compose logs api solver | Select-String <request id>'
} finally {
    Pop-Location
}
