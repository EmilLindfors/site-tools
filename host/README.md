# The publisher on mail.lindfors.no

Posts written ahead do not sit in the repo, which is public. They sit in a queue on the
box, and `site-tools publish`, run there every hour from cron as the `publisher`
account, moves the next one into a clone of the site on its week's slot: dates it,
drops `draft`, makes the derived files, commits, pushes, waits for the page, and hands
the slug to the newsletter binary over loopback. The workstation end is
`site-tools schedule`. The rules are in `src/publish.rs`; this file is the install.

## What is where on the host

| Piece | Path | Owner |
|---|---|---|
| The binary, built without `cite` | `/opt/lindfors-publisher/site-tools` | root, 0755 |
| Config: cadence, paths, the send command | `/etc/lindfors-publisher.toml` | root, 0644 |
| The queue `schedule` fills | `/srv/lindfors-publisher/queue/<slug>/` | publisher |
| Published entries, kept | `/srv/lindfors-publisher/published/<slug>/` | publisher |
| A clone of the site, pushable | `/srv/lindfors-publisher/site/` | publisher |
| Deploy key, write access, this repo only | `/srv/lindfors-publisher/.ssh/id_ed25519` | publisher, 0600 |
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
install -d -o publisher -g publisher -m 0750 /srv/lindfors-publisher/queue /srv/lindfors-publisher/published
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

`fetch-fonts.sh` has no exec bit in git, hence `bash`. The last line proves the config
parses, the clone reads, and the queue is empty. Done on 2026-09-03; the deploy key
was added from the workstation with `gh repo deploy-key add <pubkey> --allow-write`.

```sh
# 6. Cron: every hour, as publisher. busybox crond reads /etc/crontabs/<user>.
echo '7 * * * * /opt/lindfors-publisher/site-tools publish run >> /var/log/lindfors-publisher/publish.log 2>&1' \
  > /etc/crontabs/publisher
chmod 0600 /etc/crontabs/publisher && chown root:root /etc/crontabs/publisher
rc-service crond restart
```

Emil's own account queues posts through `sudo -u publisher`, which `NOPASSWD: ALL`
already allows; nothing else on the host changes.

## Build and copy, from the workstation

```sh
./tools/site-tools/build-host.sh
scp tools/site-tools/target/aarch64-unknown-linux-musl/release/site-tools \
    tools/site-tools/host/lindfors-publisher.toml newsletter/send-issue hetzner:/tmp/
```

`/tmp/lindfors-newsletter` on the box is a directory left from the cutover, so the
newsletter binary has to be copied under another name, e.g. `/tmp/lindfors-newsletter.bin`.

The newsletter binary needs its `send` command too, which arrived with it in the same
change: `./newsletter/build.sh`, copy, `rc-service lindfors-newsletter restart`.

## Day to day

```sh
site-tools schedule <slug>                    # next free week; --week 2026-W41 pins one
site-tools schedule <slug> --no-send          # publish without an issue
site-tools schedule list                      # the queue, and what the next run picks
site-tools schedule remove <slug>
```

On the box, the same binary answers `publish next` (a dry run), `publish run --now`
(ignore the hour), `publish run --force` (ignore the one-per-week rule), and
`publish unqueue <slug>`.

A post is queued when it is finished: linted, cited (`site-tools cite all`; `schedule`
refuses a post with a marker left), hero and card made, `draft = true` still set. The
publisher removes the flag, sets `date` to the day it runs, and archives the entry after
the push. `git pull` afterwards replaces the local draft with the published copy at the
same path.

## What can go wrong

- **The push fails** (someone pushed at the same moment, the key is gone): the run
  exits non-zero, the entry stays queued, nothing is mailed, and the next hour's run
  resets the clone and tries again.
- **The page never answers 200** within `wait_minutes`: the post is published and
  archived, the mail is not sent, and the log says to send by hand:
  `sudo /opt/lindfors-newsletter/send-issue <slug>`. The `sends` table stops a double.
- **The send is partial**: the newsletter's own log names the addresses; `send-issue
  <slug> --catch-up` retries the ones without a delivery.
- **A week gets two posts**: it cannot from here. The run counts every post dated in
  the current ISO week, published by hand or not, against `max_per_week`.
- **A series would share a date**: refused before anything is written.
