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
TROUVE_VERSION=4.0.0
TROUVE_REVIEW_PORT=7433
TROUVE_CODE_REVIEW_POLL_INTERVAL_SECONDS=60
TROUVE_CODE_REVIEW_TIMEOUT_SECONDS=900
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
6. Choose an explicit review model and persona-routing strategy, then set each
   repository to **Manual** or **Automatic**.

`Manual` runs only when the bot is selected (or re-requested) through
GitHub's reviewer UI. `Automatic` reviews every new non-draft base/head
revision and also honors reviewer re-requests. Mentions are intentionally not
triggers.

## Reviewers

Each reviewer is one focused model pass over the pull request. trouve ships
built-in reviewers for correctness, security, reliability, performance,
concurrency, API compatibility, data integrity, testing, maintainability,
dependencies, accessibility, and operations.

Each repository has one of three persona-selection strategies:

- **Manual** runs exactly the checked personas on every diff batch. Use it when
  a repository needs a small, fixed reviewer set.
- **Additive** always runs the correctness, security, and testing baseline plus
  explicitly enabled core personas. Optional semantic triage makes one
  lightweight, read-only model pass per batch (fully tool-free when the
  backend supports it) and may add more personas; it can never remove a
  baseline or enabled core persona. This is the default.
- **Automatic** makes semantic triage mandatory and lets it select from the
  complete persona catalog for each batch, with no preselected personas.

Both Additive and Automatic honor repository-specific model, thinking, and
prompt overrides for any persona that runs. **Always run** and the repository's
included persona list force a persona into every Additive batch. Automatic
ignores **Always run** and both included and excluded persona lists: the lists
are cleared when Automatic is saved, so semantic triage remains the sole
selector. Custom reviewer profiles are reusable across repositories and contain
a name, focused prompt, and optional model override. Additive can select a
custom persona through semantic triage or **Always run**; Automatic can select
it only through semantic triage. Automatic has no fixed baseline, though the
correctness, security, and testing personas remain available in its complete
semantic-routing catalog.

New repositories start in Additive with semantic triage enabled. During upgrade,
a repository that still exactly matches either historical built-in default set
is migrated to Additive once. Customized reviewer sets remain Manual, and a
later explicit switch back to Manual is preserved.

An enabled repository must select an explicit review model. The coordinator
and every reviewer without a profile or repository override use that model;
the unattended review system never falls back to trouve's built-in thread
model because that provider/model may not be configured on the deployment.

Semantic triage has separate optional **router model** and **router thinking
level** controls. When the router model is unset it inherits the required
repository review model. When its thinking level is unset it inherits the
review mode's thinking default. Both values are snapshotted on the job, so a
policy edit cannot change an in-flight router pass.

Repository policies can refine each reviewer without changing its reusable
profile. In Core, the checkbox enables or disables that reviewer for the
repository. A model override can select a different model for that reviewer;
otherwise it inherits the profile model, then the required repository review
model. Prompt behavior can inherit the profile prompt, append
repository-specific instructions to it, or replace it for that repository.
Overrides remain saved when a reviewer is temporarily disabled or routed out.

## Runtime behavior

A reconciliation poll runs at startup and every 60 seconds by default. It can
be the only trigger source, or serve as a fallback when webhooks provide the
fast path. Set `TROUVE_CODE_REVIEW_POLL_INTERVAL_SECONDS` to any positive number
of seconds and restart the server container to change the interval. Invalid and
zero values fall back to 60 seconds. Polling uses lightweight PR metadata and
durable deduplication. Once a base/head revision has a queued, running, failed,
or published automatic-equivalent attempt, polling does not start another
automatic pass for it, including after a repository policy change. Draft-only
manual reviews and stale or cancelled attempts do not suppress the next
automatic review. An explicit dashboard retry, persona retry, reviewer
re-request, or trusted `@trouve-ai review` comment may intentionally run the
same revision again. Every newly started review snapshots the repository's
current configuration, regardless of how it was triggered. Retries retain the
predecessor's base/head revision. The persona retry control validates the
selected failed persona, then starts a fresh whole-job replacement so
successful tasks from an older settings snapshot are not mixed with current
settings. Every reviewer selected by the current policy runs again.

Each job fetches the exact base and head commits into a managed repository and
creates an isolated trouve session at that head. The complete diff is enumerated
by changed path and divided into bounded per-file batches. Manual sends every
selected reviewer every batch. Additive and Automatic record a decision for
every persona/batch candidate and dispatch only the selected combinations in
the built-in read-only review mode, including files beyond the model-facing
aggregate diff limit. Reviewer profiles, review/router models, router thinking
level, routing mode, inclusion controls, and every typed routing reason are
snapshotted durably with the job after repository overrides are applied.
The dashboard exposes both the router task output and the complete
selected/skipped decision matrix, which is also published on the job's
persisted event stream.

If semantic triage is disabled or its model response fails validation,
Additive continues with its baseline and enabled core personas. Automatic
requires semantic triage and fails the review if routing fails. Semantic output
is restricted to the offered persona IDs and requires a concrete reason. A
once-persisted routing snapshot is reused by interrupted-job recovery and
recovery. User-initiated retries create a new snapshot from the current
repository settings.

Candidate findings are first checked against actual commentable diff lines. A
separate final editor pass then verifies them against the repository, removes
false positives and findings not introduced by the revision, merges semantic
duplicates, corrects line metadata, and produces the published summary. The
result is checked against diff lines again before it is sent to GitHub.

Each job snapshots the effective review configuration when it is queued. Later
settings changes apply to newly queued jobs without changing or cancelling
existing work. When either commit changes, queued reviews for the old revision
are marked stale and an in-flight model turn is cancelled before the
replacement is queued. Before publishing, trouve reads the PR again and marks
the job stale if either commit moved. Inline findings that GitHub still rejects
are preserved in a summary-only fallback review.

The dashboard displays the most recently observed installation rate-limit
remainder and reset time. Its 15-second UI refresh only talks to the local
server and consumes no GitHub requests.

### Model-provider concurrency

Review jobs may prepare up to 24 reviewer tasks concurrently, and two jobs may
run at once, but the shared turn scheduler applies stricter gates before any
model request starts. By default, at most 26 turns run globally, at most 24 of
them may be background turns, at most 18 turns use the same provider, and at
most 16 of those may be background turns. Consequently one provider receives
no more than 16 concurrent review requests, background work across all
providers is capped at 24, and two global plus two per-provider slots remain
available for interactive work.

These are concurrency limits, not requests-per-minute guarantees; provider
plans and model-specific quotas vary. Deployments that observe throttling
should lower `TROUVE_PROVIDER_TURN_CONCURRENCY` and
`TROUVE_PROVIDER_BACKGROUND_TURN_CONCURRENCY`. The corresponding global
overrides are `TROUVE_TURN_CONCURRENCY` and
`TROUVE_BACKGROUND_TURN_CONCURRENCY`; review orchestration can be narrowed
further with `TROUVE_CODE_REVIEW_JOB_CONCURRENCY` and
`TROUVE_CODE_REVIEW_TASK_CONCURRENCY`. All limits must be positive and require
a server restart. Review-job concurrency has a hard maximum of 32; larger
persisted, API, or `TROUVE_CODE_REVIEW_JOB_CONCURRENCY` values are reduced to
32 with a server warning.

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
