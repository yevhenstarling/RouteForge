# RouteForge
backend for managing parcel pickup and delivery across a city

It turns a day's parcel orders into optimized courier routes, and spreads the route optimization over workers that scale horizontally.

## Running it

Docker with Compose is all it needs:

```sh
docker compose up --build --scale solver=4
```

The first build compiles everything and takes several minutes. On Windows, run it from PowerShell: from `cmd` or Git Bash in a terminal of VS Code, Compose 5 can refuse to build with `additional privileges requested`, and the [known traps](docs/plan.md#known-traps) say why and what helps. After that:

- the map is at <http://127.0.0.1:8080/>
- Swagger UI is at <http://127.0.0.1:8080/docs/>
- the admin is `admin@example.com`, with the password `routeforge`

On a new database the page offers an admin "Set up a demo city". Press it, generate orders, and press Plan: the routes are drawn while the solvers search for them.

That login, and the key that signs the tokens, are in this repository for everybody to read. They are for a demonstration on your own machine, which is the only place the ports are open to. Set your own in a `.env` before opening them to anything else: `.env.example` lists every setting.

## The demonstration

```sh
scripts/demo.ps1    # Windows
scripts/demo.sh     # elsewhere; needs curl and jq
```

It brings the stack up if it is not, and shows two things:

- **Scaling.** The same day is planned with one solver and then with four. The routes are the same, and they are there sooner.
- **Failure.** A solver is killed in the middle of a route. When its lease has run out, another solver takes the route over, and every order is still on exactly one route.

It plans tomorrow, so it stays out of the way of what you do on the page, which opens on today.

## The bot

The stack has a Telegram bot in it. On a clone it has no token, so it says that it idles, and does. To have it take orders:

1. Ask [@BotFather](https://t.me/BotFather) in Telegram for a new bot, and put the token it gives into `.env`: `TELEGRAM_BOT_TOKEN=...`
2. `docker compose up -d --build`
3. Open a chat with the bot and send `/start`.

To place an order, share a location: the paperclip, then Location. The bot asks with buttons whether it is a delivery or a pickup, how many parcels, and which day. It shows the order, and places it when you confirm. Where a location cannot be shared, `/order 50.4501 30.5234` begins an order at those coordinates. `/orders` lists your orders, to look at or to withdraw.

With the page open on that day as the admin, the order is on the map as it is placed.

Whoever writes to the bot gets an account of their own, a client's. To plan from the chat, make that account an operator's: `/start` says its id, and `PUT /api/users/{id}/role` in Swagger UI changes its role, as the admin. An operator has `/pending`, `/plan` and `/status` as well.

The bot takes buttons and commands, and no free text. The [approved description](docs/description.md) has it understand sentences, with the help of the Claude API. That part waits for an API key to try it with. It goes on top of this one: what a button does and what a model would ask for is the same request to the API. The reasons are D58 in [docs/architecture.md](docs/architecture.md).

The days the bot offers are those of its clock, which is on UTC in a container unless `TZ` in `.env` says otherwise. The buttons name the day they mean.

## More

- [docs/description.md](docs/description.md): what the project is to be
- [docs/plan.md](docs/plan.md): the phases, and what each delivered
- [docs/architecture.md](docs/architecture.md): how it is built, and why
