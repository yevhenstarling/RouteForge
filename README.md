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

## More

- [docs/description.md](docs/description.md): what the project is to be
- [docs/plan.md](docs/plan.md): the phases, and what each delivered
- [docs/architecture.md](docs/architecture.md): how it is built, and why
