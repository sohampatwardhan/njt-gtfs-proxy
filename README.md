# njt-gtfs-proxy

Local proxy that turns NJ Transit's token-authenticated GTFS APIs into plain GET URLs.

GTFS clients only know how to GET a static zip and GTFS-Realtime protobufs. NJ Transit's rail and bus developer APIs instead require `POST` with `multipart/form-data` and a short-lived token. This process holds your credentials, caches the token, and serves the feeds.

NJ Transit's data terms apply. Register at [developer.njtransit.com](https://developer.njtransit.com/registration) and use the username and password from the email they send you. Do not commit those credentials, tokens, or downloaded feeds.

## What it serves

| GET | Upstream | Content-Type |
| --- | --- | --- |
| `/healthz` | none | `application/json` |
| `/rail/gtfs.zip` | rail `getGTFS` | `application/zip` |
| `/rail/trip-updates.pb` | rail `getTripUpdates` | `application/x-protobuf` |
| `/rail/vehicle-positions.pb` | rail `getVehiclePositions` | `application/x-protobuf` |
| `/rail/alerts.pb` | rail `getAlerts` | `application/x-protobuf` |
| `/bus/gtfs.zip` and the three `/bus/*.pb` paths | bus G2 equivalents | same types |

A rail route exists only when both `NJT_RAIL_USERNAME` and `NJT_RAIL_PASSWORD` are set. Bus is the same with the bus variables. Anything else returns 404. `/healthz` reports which side is enabled and does not require the access key.

## Configure

The process reads environment variables only.

| Variable | Default | Meaning |
| --- | --- | --- |
| `NJT_ENV` | `production` | `production` or `test`. Test uses `testraildata.njtransit.com` and `testpcsdata.njtransit.com`. |
| `NJT_RAIL_USERNAME` / `NJT_RAIL_PASSWORD` | unset | Rail credentials. Both are required to enable rail. |
| `NJT_BUS_USERNAME` / `NJT_BUS_PASSWORD` | unset | Bus G2 credentials. Both are required to enable bus. |
| `NJT_RAIL_BASE_URL` / `NJT_BUS_BASE_URL` | from `NJT_ENV` | Origin only, such as `https://raildata.njtransit.com`. No path, query, or userinfo. |
| `NJT_PROXY_BIND` | `127.0.0.1:8080` | Listen address. |
| `NJT_TOKEN_CACHE` | unset | JSON file for tokens. Created mode `0600` on Unix. A restart reuses the token instead of spending another rail mint. |
| `NJT_RT_TTL_SECS` | `20` | Realtime response cache. `0` disables it. Maximum 3600. |
| `NJT_STATIC_TTL_SECS` | `21600` | Static zip cache (6 hours). `0` disables it. Maximum 7 days. |
| `NJT_PROXY_ACCESS_KEY` | unset | When set, feed requests must send header `x-njt-proxy-key`. |

Rail and bus are different hosts. Test and production credentials are issued separately. A token from one is never sent to the other.

The listen address defaults to localhost. Binding a public interface shares your NJ Transit quota and schedule data with anyone who can open the URL. If the proxy is reachable beyond the machine, set `NJT_PROXY_ACCESS_KEY`.

Tokens are minted lazily. Rail tokens are reused until the next America/New_York midnight, which is when NJ Transit resets the 10-calls-per-day `getToken` budget. Bus tokens are reused for 24 hours. Either token is replaced when a data call reports an invalid token. Minting is single-flight, persisted when `NJT_TOKEN_CACHE` is set, and hard-capped at 10 HTTP calls per API per Eastern day. A daily-limit response or the local cap becomes HTTP 503 and no further token calls are sent until midnight Eastern. `isValidToken` is never called, because it spends the same rail budget.

JSON error bodies and the literal body `Null` are treated as errors. They are not returned as zip or protobuf.

## Run locally

Rust 1.85 or newer.

```bash
export NJT_RAIL_USERNAME='your-username'
export NJT_RAIL_PASSWORD='your-password'
export NJT_TOKEN_CACHE="$HOME/.config/njt-gtfs-proxy/tokens.json"
cargo run --release --locked -p njt-gtfs-proxy
```

Add `NJT_BUS_USERNAME` and `NJT_BUS_PASSWORD` when you want bus feeds. Use `NJT_ENV=test` with the test credentials from the registration email.

## Docker

The image is multi-arch. Buildx builds each platform from the same Dockerfile.

```bash
docker buildx build --platform linux/amd64,linux/arm64 -t njt-gtfs-proxy .
docker run --rm -p 127.0.0.1:8080:8080 \
  -e NJT_RAIL_USERNAME \
  -e NJT_RAIL_PASSWORD \
  -e NJT_PROXY_ACCESS_KEY \
  -e NJT_TOKEN_CACHE=/var/lib/njt-gtfs-proxy/tokens.json \
  -v njt-tokens:/var/lib/njt-gtfs-proxy \
  njt-gtfs-proxy
```

Inside the container the server listens on `0.0.0.0:8080` so a published port works. The `-p 127.0.0.1:8080:8080` form keeps it on the host loopback. Set the access key before you publish the port on any other interface. The volume keeps the token file across restarts so a redeploy does not spend a rail `getToken` call.

## gtfs-mcp

[gtfs-mcp](https://github.com/sohampatwardhan/gtfs-mcp) (binary `gtfs-rt-mcp`) only performs plain GETs. Point it at this proxy. The server reads the `GTFS_RT_MCP_` prefix. Header values are sent on fetch and are not logged.

```bash
GTFS_RT_MCP_STATIC=http://127.0.0.1:8080/rail/gtfs.zip \
GTFS_RT_MCP_TRIP_UPDATES=http://127.0.0.1:8080/rail/trip-updates.pb \
GTFS_RT_MCP_VEHICLE_POSITIONS=http://127.0.0.1:8080/rail/vehicle-positions.pb \
GTFS_RT_MCP_ALERTS=http://127.0.0.1:8080/rail/alerts.pb \
GTFS_RT_MCP_HTTP_HEADERS='{"x-njt-proxy-key":"replace-me"}' \
gtfs-rt-mcp stdio
```

Omit `GTFS_RT_MCP_HTTP_HEADERS` when `NJT_PROXY_ACCESS_KEY` is unset. Bus feeds use the `/bus/` paths the same way. `replace-me` is not a real key.

## Layout

- `crates/njt-client` — reusable rail and bus client, token manager, and mode-`0600` cache. A second binary can depend on this crate without the HTTP server.
- `crates/njt-gtfs-proxy` — axum GET server and response cache.

Tests talk to a local mock only. They use placeholder usernames and fake tokens.

## Upstream quirks

These differ across NJ Transit's PDF, swagger, and developer-portal samples. The client follows the swagger paths and the response fields those documents agree on.

- Rail production in the PDF and at `https://raildata.njtransit.com/swagger/GTFS/swagger.json` is `https://raildata.njtransit.com`. The developer portal's rail sample curls use `https://raildata.njt.gov`. The default here is the `.njtransit.com` host. Set `NJT_RAIL_BASE_URL` to use the other one.
- Bus `authenticateUser` returns the same JSON shape as rail `getToken`: `Authenticated` and `UserToken`. Swagger names that schema `UserAuthentication`. The bus PDF leaves the response section blank.
- One bus PDF production sample spells alerts `getALerts`. Swagger lists `/api/GTFSG2/getAlerts`, and that is the path this proxy calls.
- Bus `getGTFS` documents `{"errorMessage":"GTFS error"}`. That is a data error. It does not refresh the token. `Invalid token.` does.
- The rail PDF says to call `getToken` once per day and that the counter resets at midnight. The developer portal also describes rail tokens as valid for "hours" without a number. Cached rail tokens expire at the next America/New_York midnight, and sooner when a data call rejects them.
- `getToken` and `isValidToken` share the 10-calls-per-day rail budget. This program does not call `isValidToken`.
- Data calls are not counted against that budget, but they sometimes return HTTP 200 with JSON or the literal body `Null`. Those responses are errors.

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --bins --no-deps --document-private-items --locked
```

See `AGENTS.md`. Do not merge, tag, or publish from an agent.
