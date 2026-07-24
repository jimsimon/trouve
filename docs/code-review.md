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

Create a `.env` beside `docker-compose.review.yml` with the deployment
settings:

```dotenv
TROUVE_VERSION=3.3.3
TROUVE_REVIEW_PORT=7433
TROUVE_CODE_REVIEW_POLL_INTERVAL_SECONDS=60
```

For a published release, set `TROUVE_VERSION` to that release's version and
pull and start both containers:

```bash
docker compose -f docker-compose.review.yml pull
docker compose -f docker-compose.review.yml up -d
```

Images are published with each `vX.Y.Z` GitHub release to GitHub
Container Registry. The shared version makes Compose deploy matching server and
UI images. Each release also publishes `latest` for convenience and an immutable
commit-SHA tag.

To deploy a branch or commit before it has been released, check out that source
on the deployment server and build the images there. No desktop build or image
copy is required:

```bash
TROUVE_VERSION=dev docker compose -f docker-compose.review.yml up -d --build
```

Open `http://your-server:7433` and add at least one model provider. Provider
credentials, the GitHub private key, and the webhook secret are held by
trouve's secret store in the persistent `trouve-data` volume.

The dashboard and `/v1` API intentionally have no application-level login or
token for this single-user deployment. Anyone who can reach them can change
configuration and start reviews, so keep the dashboard on a trusted private
network or VPN. If it must be internet-accessible, put authentication and TLS
in front of it at the reverse proxy. The bundled nginx forwards `/v1/*` to the
private server container. If webhooks are enabled, expose only
`/github/webhooks` publicly over HTTPS and keep `/` and `/v1/*` restricted to
the private network.

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
6. Choose a default model, select its reviewers, and set each repository to
   **Manual** or **Automatic**.

`Manual` runs only when the bot is selected (or re-requested) through
GitHub's reviewer UI. `Automatic` reviews every new non-draft base/head
revision and also honors reviewer re-requests. Mentions are intentionally not
triggers.

## Reviewers

Each reviewer is one focused model pass over the pull request. trouve ships
built-in reviewers for correctness, security, reliability, performance,
concurrency, API compatibility, data integrity, testing, maintainability,
dependencies, accessibility, and operations. New and existing repository
policies start with the core correctness, security, API compatibility, and
testing reviewers selected.

Select only the reviewers relevant to a repository: each selected reviewer
examines every diff batch, so adding reviewers increases model usage and review
latency. Built-in reviewers use the repository's selected model (or the server
default). Custom reviewer profiles are reusable across repositories and contain
a name, focused prompt, and optional model override. Create and manage them in
the dashboard's **Reviewers** section, then enable them on each repository.

Repository policies can refine each reviewer without changing its reusable
profile. The checkbox enables or disables that reviewer for the repository. A
model override can select a different model for that reviewer; otherwise it
inherits the profile model, then the repository or server default. Prompt
behavior can inherit the profile prompt, append repository-specific
instructions to it, or replace it for that repository. Overrides remain saved
when a reviewer is temporarily disabled.

## Runtime behavior

A reconciliation poll runs at startup and every 60 seconds by default. It can
be the only trigger source, or serve as a fallback when webhooks provide the
fast path. Set `TROUVE_CODE_REVIEW_POLL_INTERVAL_SECONDS` to any positive number
of seconds and restart the server container to change the interval. Invalid and
zero values fall back to 60 seconds. Polling uses lightweight PR metadata and
durable deduplication; the model runs at most once for an automatic
base/head/config combination, while each reviewer re-request gets its own
generation.

Each job fetches the exact base and head commits into a managed repository and
creates an isolated trouve session at that head. The complete diff is enumerated
by changed path and divided into bounded per-file batches; every selected
reviewer receives every batch in the built-in read-only review mode, including
files beyond the model-facing aggregate diff limit. Reviewer profiles and models
are snapshotted with the durable job after repository overrides are applied.

Candidate findings are first checked against actual commentable diff lines. A
separate final editor pass then verifies them against the repository, removes
false positives and findings not introduced by the revision, merges semantic
duplicates, corrects line metadata, and produces the published summary. The
result is checked against diff lines again before it is sent to GitHub.

When either commit or the effective review configuration changes—including
reviewer selection, model overrides, or prompt overrides—queued reviews for the
old revision/configuration are marked stale and an in-flight model turn is
cancelled before the replacement is queued. Before publishing, trouve reads the
PR again and marks the job stale if either commit moved. Inline findings that
GitHub still rejects are preserved in a summary-only fallback review.

The dashboard displays the most recently observed installation rate-limit
remainder and reset time. Its 15-second UI refresh only talks to the local
server and consumes no GitHub requests.

## Backup and upgrades

The `trouve-data` Docker volume contains configuration, secrets, the SQLite
job/event log, managed repositories, and review sessions. Never copy its live
SQLite files. Quiesce both services, use SQLite's `.backup` mechanism for the
database, and archive the remaining volume data separately. The server image
includes the required `sqlite3` CLI.

The following example runs from the deployment directory and uses
[age](https://age-encryption.org/) for encryption. Set
`TROUVE_BACKUP_AGE_RECIPIENT` to a recipient managed by your secret-management
system:

```bash
set -eu
umask 077
backup_stamp=$(date -u +%Y%m%dT%H%M%SZ)
backup_stage=$(mktemp -d)
backup_container="trouve-backup-${backup_stamp}"
backup_output="trouve-backup-${backup_stamp}.tar.gz.age"

cleanup_review_backup() {
  docker rm -f "$backup_container" >/dev/null 2>&1 || true
  rm -rf "$backup_stage"
  docker compose -f docker-compose.review.yml start trouve-server review-ui
}
trap cleanup_review_backup EXIT

docker compose -f docker-compose.review.yml stop review-ui trouve-server
docker compose -f docker-compose.review.yml run \
  --name "$backup_container" --no-deps --entrypoint sh trouve-server -eu -c '
    mkdir -p /tmp/trouve-backup
    sqlite3 /var/lib/trouve/trouve.db ".backup /tmp/trouve-backup/trouve.db"
    tar --exclude=./trouve.db --exclude=./trouve.db-wal \
      --exclude=./trouve.db-shm -C /var/lib/trouve \
      -czf /tmp/trouve-backup/trouve-data-files.tar.gz .
    tar -C /tmp/trouve-backup -czf /tmp/trouve-backup.tar.gz .
  '
docker cp "$backup_container:/tmp/trouve-backup.tar.gz" \
  "$backup_stage/trouve-backup.tar.gz"
age --recipient "$TROUVE_BACKUP_AGE_RECIPIENT" \
  --output "$backup_output" "$backup_stage/trouve-backup.tar.gz"
chmod 600 "$backup_output"
```

Store only the encrypted output in backup storage. Restrict read access to the
operators responsible for recovery, protect the age private key separately,
and test restores periodically. The cleanup trap removes plaintext staging data
and restarts the services even if a backup step fails.

Upgrade by changing `TROUVE_VERSION` in `.env`, then run:

```bash
docker compose -f docker-compose.review.yml pull
docker compose -f docker-compose.review.yml up -d
```

The release container runs as UID/GID 10001. If a pre-release root-running
image created the existing volume, migrate its ownership once before upgrading:

```bash
docker compose -f docker-compose.review.yml run --rm --user root \
  --entrypoint chown trouve-server -R 10001:10001 /var/lib/trouve
```
