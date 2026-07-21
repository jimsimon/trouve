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
TROUVE_REVIEW_PORT=7433
TROUVE_CODE_REVIEW_POLL_INTERVAL_SECONDS=60
```

Then build and start both containers:

```bash
docker compose -f docker-compose.review.yml up -d --build
```

Open `http://your-server:7433`, enter `TROUVE_AUTH_TOKEN`, and add at least
one model provider. The token is kept in browser session storage, so closing
the tab signs the dashboard out. Provider API keys, the GitHub private key,
and the webhook secret are held by trouve's secret store in the persistent
`trouve-data` volume.

The compose file exposes plain HTTP. Put it behind your existing TLS reverse
proxy before exposing it to the internet. Route both `/v1/*` and
`/github/webhooks` to the review UI container; its bundled nginx forwards
those paths to the private server container.

## Create the bot identity

Create a new GitHub App under **Settings → Developer settings → GitHub Apps**.
It is distinct from the OAuth App used by the desktop client.

Use these settings:

- GitHub App name: any unique name; this determines the visible
  `<slug>[bot]` reviewer account.
- Homepage URL: the public HTTPS URL of this dashboard.
- Webhook: active.
- Webhook URL: `https://YOUR_HOST/github/webhooks`.
- Webhook secret: generate a random secret and keep it for the dashboard.
- Repository permission **Contents**: Read-only.
- Repository permission **Pull requests**: Read and write.
- Subscribe to the **Pull request** event.
- Installation scope: only this account, unless the App is intended for
  other organizations too.

After creating it:

1. Note the numeric **App ID** (not Client ID).
2. Generate and download a private key (`.pem`).
3. Install the App on the repositories it may review. Selecting individual
   repositories keeps its access narrow.
4. In the trouve dashboard, enter the App ID, the complete PEM contents, and
   the same webhook secret.
5. Click **Poll now**. The installed repositories will appear with review
   mode **Off**.
6. Choose a model and set each repository to **Manual** or **Automatic**.

`Manual` runs only when the bot is selected (or re-requested) through
GitHub's reviewer UI. `Automatic` reviews every new non-draft head SHA and
also honors reviewer re-requests. Mentions are intentionally not triggers.

## Runtime behavior

Webhooks provide the fast path. A reconciliation poll runs at startup and
every 60 seconds by default so a missed delivery cannot strand a review. Set
`TROUVE_CODE_REVIEW_POLL_INTERVAL_SECONDS` to any positive number of seconds
and restart the server container to change the interval. Invalid and zero
values fall back to 60 seconds. Polling uses lightweight PR metadata and
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
with:

```bash
git pull
docker compose -f docker-compose.review.yml up -d --build
```
