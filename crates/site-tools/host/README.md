# The publisher on mail.lindfors.no

A post is written, reviewed and confirmed in the writing desk (`lindfors-services`,
`crates/lindfors-writing`, with the admin at `services.lindfors.no/admin/`). Its
confirmed approval names a revision, its content digest and the instant it goes out.
`site-tools publish`, run on the box every minute from cron as the `publisher`
account, reads those approvals from the desk's database with a read-only role, takes
the earliest due one on under the article's own lock, exports the revision and its
assets as a page bundle, dates it, drops `draft`, makes the derived files, commits,
pushes, waits for the page, and hands the slug to the newsletter binary over
loopback. Each step goes into a receipt the desk reads back. The rules are in
`src/publish.rs`; this file is the install.

Until 2026-09-29 the queue was a directory in the private lindfors-services repo,
filled by `site-tools schedule` on the workstation and by a socket gateway the desk
called. That is gone: a finished post from outside the desk is imported through the
admin's Import, and the second deploy key and clone are no longer needed.

## What is where on the host

| Piece | Path | Owner |
|---|---|---|
| The binary, built without `cite` | `/opt/lindfors-publisher/site-tools` | root, 0755 |
| Config: cadence, paths, the send command | `/etc/lindfors-publisher.toml` | root, 0644 |
| A clone of the site, pushable | `/srv/lindfors-publisher/site/` | publisher |
| Deploy key, write access, the site repo | `/srv/lindfors-publisher/.ssh/id_ed25519` | publisher, 0600 |
| The desk's read-only database credential | the `host/publisher` sec bundle, `PUBLISHER_DATABASE_URL` | root decrypts |
| Receipts, one per publication | `/srv/lindfors-publisher/receipts/` | publisher, group writing-queue, 0750 |
| The cron command: bundle, account, lock, run | `/opt/lindfors-publisher/publish-locked` | root, 0755 |
| Fonts for the PDFs | `/srv/lindfors-publisher/site/fonts/` | publisher (gitignored) |
| The one root command it may run | `/opt/lindfors-newsletter/send-issue` | root, 0755 |
| Log | `/var/log/lindfors-publisher/publish.log` | publisher |

The publisher never reads `/etc/lindfors-newsletter.env`. `send-issue` does, as root,
through a sudoers line that allows exactly that command.

## First install, as root

```sh
# 1. The account, its home, and the directories.
addgroup -S publisher
adduser -S -D -h /srv/lindfors-publisher -s /bin/sh -G publisher publisher
install -d -o publisher -g publisher -m 0750 /srv/lindfors-publisher
install -d -o publisher -g publisher -m 0755 /var/log/lindfors-publisher
install -d -m 0755 /opt/lindfors-publisher

# 2. The toolchain: git, curl, typst. The version must match the workstation's
#    (`typst --version`): the PDFs and share images are committed, and a different
#    typst re-renders every one of them on the first publish.
apk add git curl xz
TYPST=0.14.2
curl -sL "https://github.com/typst/typst/releases/download/v$TYPST/typst-aarch64-unknown-linux-musl.tar.xz" \
  | tar xJ -C /tmp && install -m 755 /tmp/typst-aarch64-unknown-linux-musl/typst /usr/local/bin/typst
typst --version

# 3. The binary and the config (built and copied from the workstation, see below).
install -m 755 /tmp/site-tools /opt/lindfors-publisher/site-tools
install -m 644 /tmp/lindfors-publisher.toml /etc/lindfors-publisher.toml

# 4. The send: the wrapper, and the sudoers line that allows it and nothing else.
install -m 755 /tmp/send-issue /opt/lindfors-newsletter/send-issue
echo 'publisher ALL=(root) NOPASSWD: /opt/lindfors-newsletter/send-issue' > /etc/sudoers.d/lindfors-publisher
chmod 0440 /etc/sudoers.d/lindfors-publisher && visudo -c

# 5. The deploy key and the clone, as publisher.
su -s /bin/sh publisher <<'EOS'
cd ~
mkdir -m 0700 -p .ssh
ssh-keygen -t ed25519 -N '' -C 'lindfors-publisher@mail.lindfors.no' -f .ssh/id_ed25519
ssh-keyscan github.com >> .ssh/known_hosts 2>/dev/null
cat .ssh/id_ed25519.pub
EOS
```

Add that public key on GitHub under the repo's *Settings -> Deploy keys* with **Allow
write access**. Then, still as `publisher`:

```sh
su -s /bin/sh publisher <<'EOS'
cd ~
git clone git@github.com:EmilLindfors/lindfors-site.git site
cd site
git config user.name  "lindfors-publisher"
git config user.email "publisher@lindfors.no"
bash scripts/fetch-fonts.sh
/opt/lindfors-publisher/site-tools publish list
EOS
```

`fetch-fonts.sh` has no exec bit in git, hence `bash`. Done on 2026-09-03; the deploy
key was added from the workstation with `gh repo deploy-key add <pubkey> --allow-write`.

The desk's database role comes from `crates/lindfors-writing/host/provision-db.sh`
in lindfors-services, which creates `writing_publisher` with `SELECT` on the desk's
tables and imports the `host/publisher` bundle. The publisher must be in the
`writing-queue` group to read the desk's assets, and the receipts directory is its own:

```sh
adduser publisher writing-queue
install -d -o publisher -g writing-queue -m 0750 /srv/lindfors-publisher/receipts
install -d -o publisher -g publisher -m 0750 /srv/lindfors-publisher/export
install -m 755 /tmp/publish-locked /opt/lindfors-publisher/publish-locked
```

```sh
# 6. Cron: once a minute, from root's crontab, because root decrypts the bundle;
#    publish-locked drops to publisher and holds the lock. Remove the old hourly
#    line from /etc/crontabs/publisher.
echo '* * * * * /opt/lindfors-publisher/publish-locked >> /var/log/lindfors-publisher/publish.log 2>&1' \
  >> /etc/crontabs/root
rc-service crond restart
```

`site-tools publish list` as publisher, under `sec exec host/publisher`, shows every
confirmed approval with its time and receipt, and what the next run would do.

## Build and copy, from the workstation

```sh
./tools/site-tools/build-host.sh
scp tools/site-tools/target/aarch64-unknown-linux-musl/release/site-tools \
    tools/site-tools/host/lindfors-publisher.toml \
    tools/site-tools/host/publish-locked \
    ../lindfors-services/crates/lindfors-newsletter/send-issue hetzner:/tmp/
```

`/tmp/lindfors-newsletter` on the box is a directory left from the cutover, so the
newsletter binary has to be copied under another name, e.g. `/tmp/lindfors-newsletter.bin`.

The newsletter binary needs its `send` command too, which arrived with it in the same
change. The service is in the lindfors-services repo since 2026-09-07: `./build.sh
newsletter` there, copy, `rc-service lindfors-newsletter restart`.

## Day to day

Nothing is run here by hand. A post is written or imported, reviewed, validated and
confirmed for a time in the writing desk; the next minute's run after that time
publishes it. On the box, under `sec exec host/publisher -- su-exec publisher`, the
same binary answers `publish list` (every confirmed approval with its time and
receipt, then what the next run would do) and `publish next` (a dry run).

`twir` on the approval costs the box nothing: the publish commit gets a
`Syndicate: this-week-in-rust` trailer and the repo's `twir` workflow opens the pull
request on the push, with the `TWIR_TOKEN` secret on GitHub. Nothing here holds a
GitHub API token.

A post is confirmed when it is finished: linted, cited (the desk's validation refuses a
marker left over), hero and card made, `draft = true` still set. The publisher removes
the flag, sets `date` to the day it runs, and pushes. `git pull` in the site checkout
afterwards.

## What can go wrong

- **The approval does not add up** (digest, validation, a missing asset): the run
  prints why and skips it; nothing is written. Fix it in the desk: a new revision, a new
  validation, a new proposal.
- **Withdrawn under the publisher**: the desk refuses a withdrawal once the receipt
  exists, and the publisher refuses an approval that changed between the read and the
  lock. Neither side can win the race the other lost.
- **The push fails** (someone pushed at the same moment, the key is gone): the run
  exits non-zero, the receipt is dropped, nothing is mailed, and the next minute's run
  resets the clone and tries again from the desk's approval.
- **The page never answers 200** within `wait_minutes`: the post is pushed, the
  receipt says `deploying`, the mail is not sent, and the next run waits again. A
  page that never comes up is sent by hand once it does:
  `sudo /opt/lindfors-newsletter/send-issue <slug>`. The `sends` table stops a double.
- **The send is partial**: the newsletter's own log names the addresses; `send-issue
  <slug> --catch-up` retries the ones without a delivery.
- **A series would share a date**: refused before anything is written.
