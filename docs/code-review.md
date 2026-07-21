# Self-hosted code review

trouve can run as an installed GitHub App, review pull requests with any
configured trouve model, and publish the result under the App's bot identity.
The dashboard is a small, separate web container; all configuration, durable
jobs, agent sessions, and GitHub access remain in `trouve-server`.

This does **not** reuse the desktop GitHub OAuth token. Account PR discovery
continues to use OAuth and continues to return every PR relevant to that user.
The review service uses installation access tokens, with a separate GitHub
rate-limit allocation for each installation.

## Deploy

Create a `.env` beside `docker-compose.review.yml` with a long random token:

```dotenv
TROUVE_AUTH_TOKEN=replace-with-at-least-32-random-bytes
TROUVE_VERSION=2.1.0
TROUVE_REVIEW_PORT=7433
TROUVE_CODE_REVIEW_POLL_INTERVAL_SECONDS=60
```

For a published release, set `TROUVE_VERSION` to that release's version and
pull and start both containers:

```bash
docker compose -f docker-compose.review.yml pull
docker compose -f docker-compose.review.yml up -d
```

Images are published with each `trouve-search-vX.Y.Z` GitHub release to GitHub
Container Registry. The shared version makes Compose deploy matching server and
UI images. Each release also publishes `latest` for convenience and an immutable
commit-SHA tag.

To deploy a branch or commit before it has been released, check out that source
on the deployment server and build the images there. No desktop build or image
copy is required:

```bash
TROUVE_VERSION=dev docker compose -f docker-compose.review.yml up -d --build
```

Open `http://your-server:7433`, enter `TROUVE_AUTH_TOKEN`, and add at least
one model provider. The token is kept in browser session storage, so closing
the tab signs the dashboard out. Provider API keys, the GitHub private key,
and the webhook secret are held by trouve's secret store in the persistent
`trouve-data` volume.

The compose file exposes plain HTTP. Put the dashboard behind your existing TLS
reverse proxy before exposing it to the internet, or keep it reachable only on
a trusted network or VPN. The bundled nginx forwards `/v1/*` to the private
server container. If webhooks are enabled, it also forwards
`/github/webhooks`; that route must be publicly reachable over HTTPS.

## Create the bot identity

Create a new GitHub App under **Settings → Developer settings → GitHub Apps**.
It is distinct from the OAuth App used by the desktop client.

Use these common settings:

- GitHub App name: any unique name; this determines the visible
  `<slug>[bot]` reviewer account.
- Homepage URL: the dashboard URL, or the project's repository URL when the
  dashboard is private.
- Callback URL: none. Delete the empty callback entry if GitHub shows one.
- **Expire user authorization tokens**: leave enabled; it is ignored because
  this App does not request user authorization.
- **Request user authorization (OAuth) during installation**: disabled.
- **Enable Device Flow**: disabled.
- Setup URL: blank (or the dashboard URL as an optional convenience).
- **Redirect on update**: disabled.
- Repository permission **Contents**: Read-only.
- Repository permission **Pull requests**: Read and write.
- All organization and account permissions: No access.
- Installation scope: **Only on this account** when every reviewed repository
  belongs to the App owner. Use **Any account** when the App must be installed
  on a different personal account or organization; installation still grants
  access only to the repositories selected there.

Then choose one trigger setup:

### Polling only

This is the simplest option and does not require a public inbound route:

- Webhook **Active**: disabled.
- Webhook URL and secret: blank.
- Subscribe to events: select nothing.
- In the trouve dashboard, leave **Webhook secret** blank.

The server reconciles GitHub at startup and at the configured polling interval.

### Webhook plus polling

Use this when reviews should start immediately and the dashboard has a public
HTTPS endpoint:

- Webhook **Active**: enabled.
- Webhook URL: `https://YOUR_HOST/github/webhooks`.
- Webhook secret: generate a strong random value and enter the same value in
  the trouve dashboard.
- Subscribe to the **Pull request** event only. GitHub may not show this event
  until the Pull requests repository permission is selected.

Polling remains enabled as a fallback for missed webhook deliveries.

After creating it:

1. Note the numeric **App ID** (not Client ID).
2. Generate and download a private key (`.pem`).
3. Install the App on the repositories it may review. Selecting individual
   repositories keeps its access narrow.
4. In the trouve dashboard, enter the App ID and the complete PEM contents.
   Leave the webhook secret empty for polling-only operation, or enter the
   GitHub webhook secret when webhooks are enabled.
5. Click **Poll now**. The installed repositories will appear with review
   mode **Off**.
6. Choose a model and set each repository to **Manual** or **Automatic**.

`Manual` runs only when the bot is selected (or re-requested) through
GitHub's reviewer UI. `Automatic` reviews every new non-draft head SHA and
also honors reviewer re-requests. Mentions are intentionally not triggers.

## Runtime behavior

A reconciliation poll runs at startup and every 60 seconds by default. It can
be the only trigger source, or serve as a fallback when webhooks provide the
fast path. Set `TROUVE_CODE_REVIEW_POLL_INTERVAL_SECONDS` to any positive number
of seconds and restart the server container to change the interval. Invalid and
zero values fall back to 60 seconds. Polling uses lightweight PR metadata and
durable deduplication; the model runs at most once for an automatic head/config
combination, while each reviewer re-request gets its own generation.

Each job fetches the exact base and head commits into a managed repository,
creates an isolated trouve session at that head, and runs the built-in
read-only review mode. Before publishing, trouve reads the PR again and marks
the job stale if the head moved. Inline findings that GitHub rejects are
preserved in a summary-only fallback review.

The dashboard displays the most recently observed installation rate-limit
remainder and reset time. Its 15-second UI refresh only talks to the local
server and consumes no GitHub requests.

## Backup and upgrades

Back up the `trouve-data` Docker volume. It contains configuration, secrets,
the SQLite job/event log, managed repositories, and review sessions. Upgrade
by changing `TROUVE_VERSION` in `.env`, then run:

```bash
git pull
docker compose -f docker-compose.review.yml pull
docker compose -f docker-compose.review.yml up -d
```

The release container runs as UID/GID 10001. If a pre-release root-running
image created the existing volume, migrate its ownership once before upgrading:

```bash
docker compose -f docker-compose.review.yml run --rm --user root \
  --entrypoint chown trouve-server -R 10001:10001 /var/lib/trouve
```
