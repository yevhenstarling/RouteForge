# RouteForge

A backend for managing parcel pickup and delivery across a city, made as the capstone of a Rust bootcamp. It turns a day's parcel orders into optimized courier routes, and spreads the route optimization over workers that scale horizontally:

- **an API** in actix-web, for orders, accounts and plans, with OpenAPI and Swagger UI;
- **solvers**, as many as are started. They take their jobs from a queue in PostgreSQL, and one that dies leaves its job to the others;
- **a page** with a Leaflet map, on which a plan is drawn while the solvers search for it;
- **a Telegram bot**, which places orders through the API.

## Running it

Docker with Compose is all it needs:

```sh
docker compose up --build --scale solver=4
```

The first build compiles everything and takes several minutes. On Windows, run it from PowerShell: from `cmd` or Git Bash in a terminal of VS Code, Compose 5 can refuse to build with `additional privileges requested`, and the [known traps](docs/plan.md#known-traps) say why and what helps. After that:

- the map is at <http://127.0.0.1:8080/>
- Swagger UI is at <http://127.0.0.1:8080/docs/>
- the admin is `admin@example.com`, with the password `routeforge`

On a new database the page offers an admin "Set up a demo city". Press it, generate orders, and press Plan: the routes are drawn while the solvers search for them. The orders are put at random on the streets of Kyiv, within 10 km of the depot.

What there is to see on the page:

- **A plan as it is made.** A route is a dashed line while a solver searches for it, and a solid one when it is stored. A small chart in its row shows its length falling as the search goes on. With 300 orders a route is at its final length a quarter of a second in, and the chart is one step. With 1200 it goes on falling for seconds.
- **Vans that drive the routes** of a plan that is done. They are simulated. Each page moves its own along the stored routes, at a speed at which the longest route takes a minute. Nothing tracks a courier, and the project is not to.
- **A route sheet** for each stored route: its stops in the order the courier calls, with what is on board after each. A click on a route's line opens its sheet, and a row of the sheet shows its stop on the map.
- **The queue and the workers,** as they are, and the jobs that ended in the last day.
- **A plan that was asked for elsewhere,** by chat, by a script or in another window: a page that shows its day shows the plan as it is made.

And what a client has, in a second window:

- **An account.** "Open an account" on the login makes one, with an address and a password of your choosing. It is a client's: it places orders and sees its own.
- **An order by a click on the map,** for the day that is shown. An operator and the admin can place one that way too.
- **A list of your orders,** which says what has become of each: pending, planned or withdrawn. A pending one can be withdrawn there. It hears of all that by itself.

With the admin's window on the same day, the order is on the admin's map as it is placed. Plan the day there, and the client's order goes from pending to planned, and then says who has it: `Van 3 has it: stop 12 of 86`. That is all a client is told of a route.

An order is placed once. The page and the bot send a key with it, so when the answer is lost on its way back, the order can be asked for again and the API answers with the one it has.

That login, and the key that signs the tokens, are in this repository for everybody to read. They are for a demonstration on your own machine, which is the only place the ports are open to. Set your own in a `.env` before opening them to anything else: `.env.example` lists every setting. Until you do, the API says so in its log every time it starts.

## The demonstration

```sh
scripts/demo.ps1    # Windows
scripts/demo.sh     # elsewhere; needs curl and jq
```

It brings the stack up if it is not, and shows two things:

- **Scaling.** The same day is planned with one solver and then with four. The routes are the same, and they are there sooner.
- **Failure.** A solver is killed in the middle of a route. When its lease has run out, another solver takes the route over, and every order is still on exactly one route.

It plans tomorrow, so it stays out of the way of what you do on the page, which opens on today.

This is what it showed on the development machine, which has 16 logical processors and runs Docker Desktop on WSL2. The day had 300 orders on four vans, and every route was searched for 200 million moves:

| Solvers | Seconds | Speed-up | Kilometres |
|---|---|---|---|
| 1 | 15.4 | 1.0 | 226.70 |
| 2 | 8.2 | 1.9 | 226.70 |
| 4 | 4.5 | 3.4 | 226.70 |
| 8 | 4.6 | 3.3 | 226.70 |

- The routes were the same in every run, stop for stop.
- Four solvers are 3.4 times as fast as one, and not four times. A search that had the machine to itself took 3.8 seconds, and each of four side by side took 4.5.
- Eight solvers are no faster than four. The day has four routes.

In the failure run a solver was killed two seconds into its route. Another solver took the route over 27.9 seconds after the kill, when the lease of the dead one had run out. The plan was done 40.4 seconds after it was asked for, with each of its 300 orders on exactly one route.

`scripts/demo.ps1 scaling -Solvers 1,2,4,8` prints the table for your machine.

### Trying the page

```sh
node scripts/page-check.mjs    # needs Node 22 or later, and Edge or Chrome
```

It drives a browser without a window through what the page is for, against the stack: the admin sets up the demo city if there is none, gives a day its orders, places one by a click, plans the day, opens a route sheet and discards the plan. Then somebody opens an account, places an order by a click and withdraws it. The browser has to hold nothing against the page all the while: no error, no refused script, no request to another host. It plans a day a month ahead, `--day` for another, and `--url` points it at another API. The admin is the one `ADMIN_EMAIL` and `ADMIN_PASSWORD` name, or the demonstration's.

## How a route is found

A plan is made in two steps, and each step is a job on the queue.

1. **Clustering.** One job sorts the day's orders into a group for each courier. It sweeps round the depot and fills the largest courier first, then finds room for what the sweep passed over. A group is no more than its courier can carry out, and no more than it can carry back.
2. **A search for each courier.** Each is a job of its own, so as many routes are searched for at once as there are solvers:
   - nearest neighbour builds a first tour;
   - 2-opt and Or-opt improve it until neither finds a move;
   - simulated annealing goes on from there until the budget of the plan is used up, and keeps the best tour it saw.

A courier leaves with its deliveries on board and comes back with its pickups. The search takes up no tour on which the courier would carry more than it can at any stop.

The search is deterministic. The same orders, the same seed and the same budget of moves give the same routes, with one solver or with eight. A budget of time cuts the same search short where the clock says.

### What the moves buy

`cargo run -p routeforge-tsp --release --example quality` searches under budgets of moves and prints how long the routes come out. The lengths below are means over eight seeds. The times are those of the development machine, which makes about fifty million moves a second on one core.

How far above the shortest tour a search ends, on three instances of TSPLIB whose shortest tour is published:

| | eil51, 50 stops | berlin52, 51 stops | kroA100, 99 stops | It took |
|---|---|---|---|---|
| Nearest neighbour | 20.0 % | 19.1 % | 30.7 % | under 0.1 ms |
| 2-opt and Or-opt, to their end | 0.7 % | 5.3 % | 2.5 % | 0.3 to 2 ms |
| The whole search, 1 million moves | 0.4 % | 0.0 % | 0.4 % | 20 ms |
| 10 million moves | 0.03 % | 0.00 % | 0.01 % | 0.2 s |
| 100 million moves | 0.00 % | 0.00 % | 0.00 % | 2 s |

A random day on four couriers has no published answer. This is how far above the shortest total that any of the searches found:

| | 300 stops, routes of 49 to 85 | 1200 stops, routes of 220 to 331 |
|---|---|---|
| Nearest neighbour | 34.7 % | 23.3 % |
| 2-opt and Or-opt, to their end | 2.2 % | 3.7 % |
| The whole search, 1 million moves | 0.6 % | 3.9 % |
| 10 million moves | 0.03 % | 2.0 % |
| 100 million moves | 0.00 % | 0.7 % |

A budget counts every move, those of 2-opt and Or-opt as well. On a route of three hundred stops they take more than a million before annealing begins, which is why a million moves are no better there than 2-opt and Or-opt alone. The API gives a route ten seconds unless the plan says otherwise, which is some five hundred million moves.

### What a stage takes

`cargo bench -p routeforge-tsp`, on the development machine, on random stops:

| | 50 stops | 200 stops | 1000 stops |
|---|---|---|---|
| Clustering a day | 22 µs | 85 µs | 0.44 ms |
| Nearest neighbour | 2.3 µs | 34 µs | 0.95 ms |
| 2-opt, from there to its end | 9.2 µs | 0.25 ms | 9.2 ms |
| Or-opt, from there to its end | 0.42 ms | 9.8 ms | 0.55 s |
| Annealing, 100 000 moves | 2.1 ms | 2.1 ms | 4.2 ms |

Minding what the courier carries costs next to nothing. The whole search for a route, ten million moves, took 194 ms without it and 195 ms with it at 50 stops, and 166 ms and 169 ms at 300.

`cargo run -p routeforge-tsp --release --example stages` plans a random day and prints its length after each stage.

## Distances by road

On a clone the solvers measure along great circles, and the stack asks nobody for anything. With an [OSRM](https://project-osrm.org/) server beside them, they measure along the roads. The server is in the compose file, behind a profile, and needs the roads prepared once:

```sh
scripts/osrm-prepare.ps1    # Windows
scripts/osrm-prepare.sh     # elsewhere; needs curl
```

That fetches the roads of Kyiv, 34 MB, into `osrm/` and prepares them. The first time it fetches OSRM's image as well, some 400 MB. With the image there it took 16 seconds on the development machine. Then put two lines into `.env`:

```sh
COMPOSE_PROFILES=osrm
OSRM_URL=http://osrm:5000
```

and start the stack as before. The page then writes `by road` beside the length of each route, where it wrote `as the crow flies`, and draws each stored route along the streets it takes. The vans drive along them too.

What it changes, for the demonstration's day of 300 orders on four vans:

| | As the crow flies | By road |
|---|---|---|
| The routes that were found on great circles | 227 km | 572 km |
| The routes that were found by road | 258 km | 476 km |

The routes that look shortest on the map are a fifth longer to drive than the ones that were searched for by road.

By road everything is two to two and a half times the straight line. The stops are some 750 metres apart, and over such a distance one-way streets and turns that are not allowed count for much. The Dnipro runs through the middle, with few bridges. A demo order is on a street, so the server moves none of them far onto its roads: the farthest by 25 metres.

A route that the server gives no distances for is searched for on great circles, and says `as the crow flies`. So a plan is done whether the server is there or not. Stopped in the middle of a plan, it left one route by road and three as the crow flies.

The way a route takes is asked of the server once, when the route is stored, and kept with it. A route by road that the server gave no way for is drawn from stop to stop, and says so.

The roads are OpenStreetMap's, © OpenStreetMap contributors, under the Open Database Licence, as [BBBike](https://download.bbbike.org/osm/bbbike/) extracts them. They are not part of this repository. The streets that demo orders are put on are: `node scripts/streets.mjs` takes them out of the same extract, as a point every 25 metres, into `crates/routeforge-api/data/kyiv-streets.bin`, under the same licence.

## The bot

The stack has a Telegram bot in it. On a clone it has no token, so it says that it idles, and does. To have it take orders:

1. Ask [@BotFather](https://t.me/BotFather) in Telegram for a new bot, and put the token it gives into `.env`: `TELEGRAM_BOT_TOKEN=...`
2. `docker compose up -d --build`
3. Open a chat with the bot and send `/start`.

To place an order, share a location: the paperclip, then Location. The bot asks with buttons whether it is a delivery or a pickup, how many parcels, and which day. It shows the order, and places it when you confirm. Where a location cannot be shared, `/order 50.4501 30.5234` begins an order at those coordinates. `/orders` lists your orders, to look at or to withdraw.

With the page open on that day as the admin, the order is on the map as it is placed.

Whoever writes to the bot gets an account of their own, a client's. To plan from the chat, make that account an operator's: `/start` says its id, and `PUT /api/users/{id}/role` in Swagger UI changes its role, as the admin. An operator has `/pending`, `/plan` and `/status` as well.

The bot can take an address in words too, if it is given a geocoding service to ask where the address is: `GEOCODER_URL` in `.env`. It then shows up to three places it found, and the one you pick begins the order. This is off on a clone, because what a person writes is then sent to that service. `.env.example` names the public service of OpenStreetMap, and what using it takes on.

The bot reads buttons, commands and, with that service, addresses. It does not understand sentences. The [approved description](docs/description.md) has it do so, with the help of the Claude API. That part was not built: there was no API key to try it with, and code for an API that was never run against it is not shipped. It would go on top of what is there, since what a button does and what a model would ask for is the same request to the API. The reasons are D58 in [docs/architecture.md](docs/architecture.md).

The bot has been tried against stand-ins for Telegram and never against Telegram itself, for want of a token. [docs/plan.md](docs/plan.md#needed-from-the-project-owner) says how to try it with one.

The days the bot offers are those of its clock, which is on UTC in a container unless `TZ` in `.env` says otherwise. The buttons name the day they mean.

## Known limits

What the system does not do, or does only up to a point:

- **One order is one stop.** A parcel goes from the depot to a client, or from a client to the depot. A courier makes one round a day, and there are no time windows.
- **A demo order is on a street, not at a door.** It may be on a main road where it crosses a park or an island, with no house beside it. Round a depot outside Kyiv the streets are not known, and demo orders are anywhere on the disc round it.
- **A day is planned once.** A day whose plan is done is planned again when that plan has been discarded, and then with every order of it. An order that comes in after the day was planned waits for that.
- **A route that is still searched for is drawn as straight lines,** by road as well. It follows its streets once it is stored. One that was stored while the OSRM server was away stays straight until its day is planned again.
- **The vans are a simulation.** No courier reports where they are. Each page reckons its own vans, so a page that opens a plan later starts them from the depot, and Refresh sends them out again.
- **The chart of a search is the page's.** Nothing stores the lengths a search announces, so a page that is opened after a search has no chart of it.
- **The route sheet has no distances.** The length of a route is stored, and that of its legs is not.
- **The bot has not met Telegram, and understands no sentences.** Both are in the section above.
- **A login is counted by its address.** After five tries in a minute the next is refused, so somebody who knows an address can keep its owner out for as long as they keep trying. The count is in the memory of the one API.
- **A token lasts until it expires.** A role that an admin changes applies from the user's next login, an hour later at the most.
- **Nothing limits the bot's endpoint,** or how many accounts it makes. The bot's key is one key, and changing it means starting both again.
- **Nothing limits how many accounts are opened on the page** either. Each costs the API one hashed password, of which it does four at a time.
- **A message that Telegram refuses for coming too fast** is logged and not sent again.
- **A client is told where their order is in the round, and not when.** A route has no times. And a client sees no route, no van and no sheet.
- **On the page an operator can place an order, and cannot withdraw one.** The API lets them. A place cannot be typed there either: it is clicked.
- **The depot and the couriers are read when the page is.** One that an admin has moved or added shows on a page that was open after Refresh.
- **One person holds sixteen event streams open at the most,** unless `API_STREAMS_PER_USER` says otherwise. A page holds two or three. Nothing limits how many accounts hold them.
- **One address search a second,** for all the chats of the bot together, which is what the public service allows.
- **A route of more than 2000 stops** is more than the OSRM server is started for, and is searched for on great circles.
- **The page is tried by a script that is run by hand,** `scripts/page-check.mjs`. The tests of the API check that it is served and what it loads, and not what it does.

## More

- [docs/description.md](docs/description.md): what the project is to be
- [docs/plan.md](docs/plan.md): the phases, and what each delivered
- [docs/architecture.md](docs/architecture.md): how it is built, and why
