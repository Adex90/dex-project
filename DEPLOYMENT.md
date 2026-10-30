# Deployment and operations

How Synthetic Capital is provisioned, released, migrated and run in production. It is
written for the infrastructure, security, DBA and compliance staff who operate the
platform, and for the auditors who review how it is operated.

- **Two topologies, the same images.** The AWS reference architecture (`terraform/`) is
  the production target. A single hardened host (`docker-compose.yml`) serves staging,
  pilots, disaster-recovery drills and on-premises sites.
- **Status of this document.** Everything in it was exercised during the change that
  introduced it, except where it says otherwise. Appendix C lists what was tested and
  how. `terraform plan` and `apply` were not run (no AWS credentials). The Terraform
  code passes `validate`, checkov, Trivy and tflint.
- **Not legal advice.** Section 11 maps controls to CNBV, ISO/IEC 27001 and SOC 2
  expectations to help the compliance team. The compliance team and the auditors must
  validate it.

Contents:
1. [Topologies and security invariants](#1-topologies-and-security-invariants)
2. [Prerequisites](#2-prerequisites)
3. [Release artifacts and CI gates](#3-release-artifacts-and-ci-gates)
4. [First deployment on AWS](#4-first-deployment-on-aws)
5. [First deployment on a single host](#5-first-deployment-on-a-single-host)
6. [Database migrations](#6-database-migrations)
7. [DNS and TLS](#7-dns-and-tls)
8. [Secrets and rotation](#8-secrets-and-rotation)
9. [Production launch checklist](#9-production-launch-checklist)
10. [Operations](#10-operations)
11. [Compliance mapping](#11-compliance-mapping)
12. [Residual risks and open items](#12-residual-risks-and-open-items)
- Appendices: [A. Configuration reference](#appendix-a-configuration-reference),
  [B. Version pins outside Dependabot](#appendix-b-version-pins-outside-dependabot),
  [C. Verification evidence](#appendix-c-verification-evidence)

---

## 1. Topologies and security invariants

### 1.1 AWS reference architecture (production)

```
internet -> WAF -> ALB (public subnets, TLS 1.3 only, HTTP -> HTTPS)
                    |- /api/*, /salud -> backend   (ECS Fargate, app subnets, :8080)
                    `- everything else -> frontend  (ECS Fargate, app subnets, :3000)
backend -> RDS Proxy (TLS) -> RDS PostgreSQL 16, Multi-AZ   (data subnets, no route out)
migrate (one-off ECS task) -> RDS instance directly, as the schema owner
```

`terraform/README.md` documents every resource, input and output. Key properties:

- **Database.** The data subnets have only the local route. The database security group
  accepts 5432 only from the RDS Proxy and from the migrate task.
- **Encryption.** Customer-managed KMS keys (yearly rotation) encrypt RDS, Performance
  Insights, the log groups, ECR and Secrets Manager.
- **Backups.** 35-day automated backups with point-in-time recovery, deletion protection
  and a final snapshot.
- **Containers.** No public IP and no task role, so the containers get no AWS
  credentials. Read-only root filesystem, uid 65532, all capabilities dropped.
- **Egress.** The backend reaches only `backend_egress_cidrs` (the provider APIs). The
  default is empty, meaning no internet egress at all.

### 1.2 Single hardened host (docker compose)

```
internet -> host 80/443 (UFW, fail2ban) -> nginx (edge network)
              web network (internal):    nginx -> frontend:3000
              api network (internal):    nginx -> backend:8080
              data network (internal):   backend, migrate -> db:5432
              egress network (internal by default): backend -> provider APIs (§5.11)
```

- **Published ports.** Only nginx publishes ports (80 and 443). The database publishes
  none and sits only on the internal `data` network, together with the backend and the
  one-shot migrate job.
- **Frontend.** The Next.js server never calls the API itself: the browser does, through
  nginx. So the frontend shares a network only with nginx, and a compromised frontend
  cannot reach the backend or the database.
- **Egress.** The backend has no internet access until a provider is integrated (§5.11),
  the same default as `backend_egress_cidrs` on AWS.
- **Limits.** One host means no high availability. A deploy recreates the backend, which
  gives about 1–2 s of `503 REINTENTAR` (the frontend translates it and the user
  retries). Recovery point: the last backup (§5.9). The AWS path has point-in-time
  recovery.
- **Data at rest.** Docker does not encrypt volumes. The host must keep `/var/lib/docker`
  and the backup staging area on an encrypted block device (LUKS, or an encrypted cloud
  volume).

### 1.3 Security invariants and where they are enforced

| Invariant | AWS | Single host | Verified by |
|---|---|---|---|
| The database has no direct internet access; only the backend (and the one-shot migrate job) reach it | Data subnets without a route out; security group matrix; NACL | No published port; internal `data` network shared only with backend and migrate | §5.7 isolation checks; Appendix C |
| Services reach only what they need | Security groups per service; backend egress limited to `backend_egress_cidrs` (default none) | Separate internal networks per path (`web`, `api`, `data`); backend egress internal by default, then restricted on the host (§5.11) | Appendix C |
| Staff authenticate with two factors; TOTP secrets never in clear | Same code on both: two-step verification (TOTP) is mandatory for `OPERADOR`, `AUDITOR` and `CUMPLIMIENTO` and re-checked on every request; secrets encrypted with `MFA_CLAVE_CIFRADO` (Secrets Manager) | Same; key in `secrets/mfa_encryption_key` | `tests/mfa.rs`, `tests/db/0010_auth_mfa.sql`, AML e2e (Appendix C) |
| Least-privilege database identities | Service holds only `sc_app` (NOINHERIT, no owner rights); owner credentials only in the migrate task | Same, plus the schema owner is **not** a superuser (`deploy/postgres/10-schema-owner.sh`); the `postgres` superuser is break-glass only | `pg_authid` query in Appendix C |
| Containers non-root, read-only, least capability, resource-limited | uid 65532, read-only rootfs, `drop ALL`, task CPU/memory | uid 65532 / 70 / 101, `read_only`, `cap_drop: ALL`, `no-new-privileges`, CPU/memory/PID limits | `docker inspect` in Appendix C |
| No secret in code, images, environment or state the operator does not control | Secrets Manager (CMK); RDS manages and rotates the owner password; Terraform state is CMK-encrypted | Docker secrets files (0400, owned by the reading uid); services read them through `NAME_FILE` | No secret value in `docker inspect` or image history (Appendix C) |
| TLS 1.3 at the edge, HSTS, strict CSP, rate limits, body-size limits | ALB policy `ELBSecurityPolicy-TLS13-1-3-2021-06`; WAF | nginx (`nginx/`) | Edge tests in Appendix C |

---

## 2. Prerequisites

### 2.1 People and segregation of duties

| Role | Holds | Never holds |
|---|---|---|
| Infrastructure administrator | AWS deployment role / host sudo; runs Terraform, compose, hardening | Compliance-officer accounts |
| DBA (schema-owner custodian) | Owner credentials for staff provisioning and break-glass | Application deploy rights, without a second approver |
| Release manager | Approves releases; checks the CI gates; signs the migration plan | Production secrets |
| Compliance officers (at least two) | `CUMPLIMIENTO` accounts in the AML console | Infrastructure access |
| Operator | `OPERADOR` account (back office) | AML console access (the app refuses it) |

- **Approvals.** Every production change (deploy, migration, rotation, staff
  provisioning) needs a ticket approved by a second person.
- **Four-eyes in the AML console.** The console's four-eyes rules are enforced by the
  database, which is why at least two compliance officers are needed.

### 2.2 External prerequisites

- **Domain and DNS.** The public host name, for example `bank.example.mx`, and the
  ability to create records. Add a CAA record restricting issuance to your CA.
- **Provider contracts:**
  - **SPEI participant (e.g. STP):** its API address ranges (for egress), its webhook
    source ranges (for the allowlist) and the webhook HMAC procedure.
  - **Biometric KYC provider:** the same items.
- **Institution data:**
  - **CLABE prefix:** bank code and plaza, plus up to 4 digits.
  - **Accounts:** the reserve custody account (IFPE, 100% reserve) and the SOFOM cash
    account.
  - **Compliance contact:** the email address shown in the app.
- **AWS only:**
  - **Account baseline:** see `terraform/README.md`, "State and account baseline".
  - **Region:** `mx-central-1`, for data residency.
  - **Tools:** Terraform 1.16.x, AWS CLI v2, `jq`.
- **Single host only:**
  - **OS:** Ubuntu 24.04 LTS (22.04 and Debian 12 are also supported by
    `scripts/hardening.sh`).
  - **Sizing:** at least 4 vCPU, 8 GB RAM and 60 GB of encrypted SSD.
  - **Software:** Docker Engine ≥ 29 with the compose plugin, and `certbot`.

### 2.3 Repository settings (GitHub)

Apply these before the first production release:

- **Protect `main`:** pull requests only, at least one approving review, dismiss stale
  approvals, no force pushes.
- **Required status checks:**
  - **CI result:** every job in `ci.yml`.
  - **Security result:** the lockfiles and the third-party images, from `security.yml`.
- **Dependabot:** enable alerts and security updates. Version updates are configured
  in `.github/dependabot.yml`.
- **Secret scanning:** enable it with push protection.
- **Actions:** restrict them to the ones the workflows use, and keep the default
  `GITHUB_TOKEN` permission read-only (the workflows request only `contents: read`).

---

## 3. Release artifacts and CI gates

| Image | Dockerfile (build context) | Base (digest-pinned) | Runs as |
|---|---|---|---|
| backend | `backend/Dockerfile` (repo root: migrations are embedded) | `gcr.io/distroless/cc-debian13:nonroot` | 65532 |
| frontend | `frontend/Dockerfile` (`frontend/`) | `gcr.io/distroless/nodejs22-debian13:nonroot` | 65532 |

- **Tags are immutable.** Use the git SHA, and never `latest`. ECR repositories reject
  overwriting a tag.
- **Promote by digest.** Promote the same digest from staging to production. The
  frontend is the exception (next bullet).
- **One frontend image per public origin.** `NEXT_PUBLIC_*` values are compiled into
  the browser bundle, so the frontend image is built once per origin. The build refuses
  a missing, relative or whitespace-containing `NEXT_PUBLIC_API_URL`.
- **Labels.** Both images carry OCI labels (`org.opencontainers.image.revision`, base
  name).

```bash
REV=$(git rev-parse HEAD)
docker build -f backend/Dockerfile --build-arg REVISION="$REV" -t <registry>/synthetic-capital-backend:"$REV" .
docker build --build-arg REVISION="$REV" \
  --build-arg NEXT_PUBLIC_API_URL=https://bank.example.mx \
  --build-arg NEXT_PUBLIC_COMPLIANCE_EMAIL=compliance@bank.example.mx \
  -t <registry>/synthetic-capital-frontend:"$REV" frontend/
```

Behind a TLS-intercepting proxy, add `--secret id=extra_ca,src=<ca.pem>`. The CA is used
only while dependencies download and never reaches a layer. Behind a pull-through
registry mirror, configure `registry-mirrors` in `/etc/docker/daemon.json`. The digests
guarantee the same bytes.

**What must be green before a release:**

- **CI result** (`ci.yml`) covers:
  - **Code checks:** lint, typecheck and unit tests (frontend); rustfmt, clippy
    `-D warnings` and every test (backend).
  - **Database and browser tests:** the six SQL invariant suites, and the Playwright
    e2e runs (smoke and AML).
  - **Images:** hadolint, then the build. The Trivy scan fails on any fixable CRITICAL
    finding and reports HIGH.
  - **Infrastructure and scripts:** Terraform fmt, validate and tflint; Trivy
    misconfiguration and secret scans; ShellCheck; actionlint.
- **Security result** (`security.yml`, also weekly): Trivy on the lockfiles and on the
  third-party images pinned in `docker-compose*.yml`.

Exceptions live only in `.github/trivyignore.yaml`. Each has a statement, an approver
and an expiry date.

---

## 4. First deployment on AWS

This section expands `terraform/README.md`, "Deploying (summary)". Run it from
`terraform/` with the deployment role.

1. **State and inputs.** The state bucket must already exist: CMK-encrypted, versioned,
   private, TLS-only, and readable only by the deployment role. The state holds the
   generated `sc_app` password.
   ```bash
   cp backend.hcl.example backend.hcl            # bucket, key, region, KMS key
   cp terraform.tfvars.example terraform.tfvars  # region, account, domain, image tags, ...
   terraform init -backend-config=backend.hcl
   ```
   Set, at least:
   - **Required:** `region`, `aws_account_id`, `domain_name`, `backend_image_tag` and
     `frontend_image_tag` (the images need not exist yet).
   - **Zones:** `availability_zones`.
   - **Business settings:** `app_environment` (CLABE prefix and custody accounts).
   - **First operator:** `bootstrap_operator_email`.
   - **Egress:** `backend_egress_cidrs`, the provider API ranges. While it is empty
     there is no egress.
2. **Certificate.** Choose one of three ways:
   - **Existing ACM certificate:** set `acm_certificate_arn`.
   - **Route 53 zone:** set `route53_zone_id`. Validation records and the alias record
     are created for you.
   - **External DNS:**
     1. Run `terraform apply -target=aws_acm_certificate.this`.
     2. Create the records from `terraform output acm_validation_records`.
     3. Continue with step 3. The apply waits up to 2 h for issuance.
     4. Then point `domain_name` at `terraform output -raw alb_dns_name`.
3. **Phase 1: infrastructure with no tasks.**
   ```bash
   terraform apply -var backend_min_tasks=0 -var frontend_min_tasks=0
   ```
   Plans download the regional RDS CA bundle from `truststore.pki.rds.amazonaws.com`.
   Where plans have no internet access, set `rds_ca_bundle_pem`.
4. **Images.** Build them as in §3, with `NEXT_PUBLIC_API_URL=https://<domain_name>` and
   for `cpu_architecture` (default `X86_64`). Push them to
   `terraform output ecr_repository_urls` with the exact tags from `terraform.tfvars`.
5. **Secrets.** Put a value in every entry of `terraform output secrets_to_populate`,
   without putting it in shell history or process arguments:
   ```bash
   openssl rand -hex 64 | tr -d '\n' | aws secretsmanager put-secret-value \
     --secret-id "$(terraform output -json secrets_to_populate | jq -r .JWT_SECRET)" \
     --secret-string file:///dev/stdin
   ```
   - **Webhook keys:** share them with each provider through the channel your contract
     defines.
   - **MFA key:** `MFA_CLAVE_CIFRADO` takes exactly 64 hex characters
     (`openssl rand -hex 32`). It encrypts the staff's TOTP secrets: keep an escrowed
     copy apart from the database backups, because losing it means every staff member
     enrolls again.
   - **ECS refuses empty secrets:** it does not start a task whose secret has no value,
     which is why the services start with zero tasks.
6. **Migrations and bootstrap.**
   ```bash
   CLUSTER=$(terraform output -raw ecs_cluster_name)
   TASK=$(eval "$(terraform output -raw migrate_run_task_command)")
   aws ecs wait tasks-stopped --cluster "$CLUSTER" --tasks "$TASK"
   aws ecs describe-tasks --cluster "$CLUSTER" --tasks "$TASK" \
     --query 'tasks[0].containers[0].exitCode'          # must be 0
   ```
   Logs are in `/ecs/<project>-<env>/migrate`.
7. **Phase 2: services.** Run `terraform apply` without the zero-task overrides.
   Autoscaling raises the services to their minimum. Then verify:
   - `https://<domain_name>/` and `/salud` answer 200 (`{"base_de_datos":"ok",...}`);
   - ALB target health;
   - the RDS Proxy target is available;
   - the WAF metrics show traffic.
8. **Providers.** Give each provider its webhook URL
   (`/api/v1/spei/webhook/...`, `/api/v1/kyc/webhook`), its HMAC key and
   `terraform output egress_public_ips`. Put their ranges in
   `spei_webhook_allowed_cidrs`, `kyc_webhook_allowed_cidrs` and `backend_egress_cidrs`,
   then apply again.
9. **Staff.** See §10.3. On AWS this needs the DBA access path that §12 lists as an
   open item.

**Where the schema owner differs on AWS.** The schema owner is the RDS master user
(`db_master_username`, default `sc_owner`). It is a member of `rds_superuser` but not a
real superuser: it cannot read server files or run programs. RDS rotates its password,
and only the migrate task can read it.

---

## 5. First deployment on a single host

Commands run as the admin account. Compose commands need either `sudo` or membership of
the `docker` group, which is root-equivalent; `sudo` is shown throughout. The scripts
that read `secrets/` (`generate-secrets.sh`, `db-client.sh`) always need `sudo`.

### 5.1 Host

- **Encrypted disk.** Install Ubuntu 24.04 LTS with full-disk encryption (or an
  encrypted cloud volume).
- **Admin user.** Create an admin user with an SSH key (ed25519) and sudo.
- **Docker.** Install Docker Engine and the compose plugin from Docker's repository, and
  `certbot`:
  ```bash
  sudo apt-get install certbot
  ```
- **Code.** Clone the repository to `/opt/synthetic-capital`, at a reviewed tag.

### 5.2 Hardening

```bash
sudo scripts/hardening.sh --admin-user alice --ssh-allow 203.0.113.0/24 --dry-run   # review
sudo scripts/hardening.sh --admin-user alice --ssh-allow 203.0.113.0/24 --yes
```

**What it sets up:**

- **Firewall:** UFW denies incoming traffic and opens 80/443. SSH is rate-limited and
  allowed only from `--ssh-allow`.
- **sshd:** keys only, no root login, `AllowUsers`, modern ciphers. The config is
  validated with `sshd -t` before any reload.
- **fail2ban:** sshd and nginx jails. The nginx jails ban in Docker's `DOCKER-USER`
  chain, because Docker's port forwarding bypasses UFW.
- **Host:** unattended security upgrades, sysctl hardening, chrony, auditd rules and
  logrotate.
- **Docker daemon:** `no-new-privileges`, `live-restore`, `userland-proxy: false`
  (nginx sees real client addresses), `icc: false`, and log rotation.
- **Deployment directory:** ownership and permissions for `secrets/`, `certs/`, `acme/`
  and `logs/nginx`.

**How it protects itself:**

- **Lockout guards:** it refuses to run if your current session could be locked out.
- **Backups:** every file it changes is backed up under `/var/backups/sc-hardening/`.
- **Idempotent:** a second run changes nothing.

**Before you start:**

- **nginx uid collision:** uid 101, nginx's uid in its image, maps to a host account
  (`sshd` on Ubuntu 24.04). That account could read the nginx logs and the TLS key. The
  script warns about this; consider `--nginx-uid` with an unused uid, and set the same
  `user:` in `docker-compose.yml`.
- **IPv6:** clients reach the published ports over IPv6 only if IPv6 is enabled on the
  edge network. Without it, do not publish an AAAA record.

### 5.3 Configuration

```bash
cd /opt/synthetic-capital
cp deploy/compose.env.example deploy/compose.env   # PUBLIC_HOST, CLABE_PREFIJO, accounts, ...
```

- **No secrets here.** `deploy/compose.env` holds no secrets. Docker compose reads it
  only to substitute `${...}` in `docker-compose.yml`.
- **Pass it every time.** Give it to every compose command as
  `docker compose --env-file deploy/compose.env ...`. Required values (`PUBLIC_HOST`,
  `CLABE_PREFIJO`, the custody accounts) make compose fail fast when the file is
  missing.
- **Production values:**
  - `MODO_DEMO=false` (the default);
  - `PUBLIC_HOST`: one host name;
  - `DB_OWNER` and `POSTGRES_DB`: fixed once the volume is initialised.
- **Directories:** `scripts/hardening.sh` creates `logs/nginx` (owned by the nginx uid),
  `certs/` and `acme/`. Without it (a throwaway staging VM, say), create them yourself.
  Otherwise Docker creates them root-owned and nginx cannot open its logs:
  ```bash
  sudo install -d -m 0750 -o 101 -g 101 logs/nginx && sudo install -d -m 0755 certs acme
  ```

### 5.4 Secrets

```bash
sudo scripts/generate-secrets.sh --env-file deploy/compose.env
```

- **Files:** it writes random values to `secrets/`, a 0700 directory. Each file is mode
  0400 and owned by the uid that reads it inside its container: 70 for PostgreSQL and
  65532 for the backend and migrate job. Values are never printed.
- **MFA key:** `mfa_encryption_key` encrypts the staff's TOTP secrets in the database.
  Losing it means every staff member must enroll again, so back it up like the rest of
  `secrets/` (and never next to the database backups).
- **Re-runs:** existing files are kept, and their permissions are restored.
- **Back up separately:** the directory is backed up separately from database backups,
  encrypted (§5.9).

### 5.5 TLS (first certificate)

nginx needs a certificate to start, and certbot needs nginx to serve the ACME challenge.
Bootstrap with a placeholder:

```bash
sudo scripts/dev-certs.sh --out certs --host bank.example.mx      # placeholder, owner 101
sudo docker compose --env-file deploy/compose.env up -d --build   # see 5.6
sudo install -m 0755 nginx/certbot-deploy-hook.sh \
  /etc/letsencrypt/renewal-hooks/deploy/synthetic-capital-nginx.sh
sudo certbot certonly --webroot -w /opt/synthetic-capital/acme -d bank.example.mx \
  --deploy-hook /etc/letsencrypt/renewal-hooks/deploy/synthetic-capital-nginx.sh
```

- **What the hook does:** it installs the certificate (key 0400, owned by the nginx
  uid) and reloads nginx gracefully, without a restart.
- **Renewals:** certbot's systemd timer handles them. Every renewal runs the hook again.
- **Where the hook looks:** `SC_DEPLOY_DIR` (default `/opt/synthetic-capital`) and
  `SC_ENV_FILE` (default `<deploy dir>/deploy/compose.env`).
- **OCSP stapling:** off, because Let's Encrypt stopped OCSP in 2025.

### 5.6 Start

```bash
sudo docker compose --env-file deploy/compose.env up -d --build --wait
```

This starts the stack in order:

1. **db.** It initialises an empty volume on first start, and
   `deploy/postgres/10-schema-owner.sh` creates the schema owner.
2. **migrate.** It applies the migrations and the bootstrap as the owner, then creates
   the `sc_app` login, and must exit 0.
3. **backend, frontend, nginx.** The backend starts only after migrate succeeded.

On a fresh volume this takes about 12 s. To run prebuilt images instead of building,
set `BACKEND_IMAGE` and `FRONTEND_IMAGE` (by digest) in `deploy/compose.env` and drop
`--build`.

### 5.7 Verify

```bash
C="sudo docker compose --env-file deploy/compose.env"
$C ps -a                                   # migrate: Exited (0); the others: healthy
curl -sS https://bank.example.mx/salud     # {"base_de_datos":"ok","estado":"ok"}
curl -sS -o /dev/null -w '%{http_code}\n' --tlsv1.2 --tls-max 1.2 https://bank.example.mx/  # fails: TLS 1.3 only
curl -sSI https://bank.example.mx/ | grep -iE 'strict-transport|content-security|x-frame'
$C exec nginx sh -c 'nc -z -w 3 db 5432 && echo REACHABLE || echo isolated'   # isolated
$C exec frontend /nodejs/bin/node -e "require('dns').lookup('backend',e=>console.log(e?'isolated':'REACHABLE'))"  # isolated
$C logs migrate | tail -3                  # "migraciones y arranque completos"
```

- **Database not exposed:** confirm from another machine that nothing answers on 5432.
- **Expected state:** the backend log says
  `la aplicación corre sin privilegios de dueño ... usuario=sc_app`, and `MODO_DEMO`
  is false (no `MODO_DEMO activo` warning).

### 5.8 Staff

- **Operator:** the migrate job creates the first operator when `BOOTSTRAP_OPERADOR_EMAIL`
  is set. Its initial password is in `secrets/bootstrap_operador_password`. Hand it
  over through a secure channel.
- **Compliance officers and auditors:** provision them as in §10.3.

### 5.9 Backups and restore

**Backup** (as the schema owner, never the superuser):

```bash
sudo scripts/db-client.sh --tool pg_dumpall -- --globals-only --no-role-passwords > globals.sql
sudo scripts/db-client.sh --tool pg_dump -- -Fc > synthetic_capital.dump
```

- **What is dumped:** the roles and their memberships, without passwords, and the whole
  database in custom format. No table forces row-level security, so the owner dumps
  everything.
- **Protect the copy:** encrypt both files before they leave the host (KMS, GPG or
  age), copy them off-site, and keep them as your retention policy requires.
- **Schedule:** at least daily, and always before a migration (§6).
- **Recovery point:** your backup interval. There is no WAL archiving on this topology.

**Restore** (tested end to end; see Appendix C):

```bash
C="sudo docker compose --env-file deploy/compose.env"
$C down && sudo docker volume rm synthetic-capital_pgdata   # only when restoring over a broken volume
$C up -d --wait db                  # init: empty cluster + schema owner
sudo scripts/db-client.sh --superuser -- -q -f - < globals.sql   # "role ... already exists" twice is expected
sudo scripts/db-client.sh --superuser --tool pg_restore -- --exit-on-error < synthetic_capital.dump
$C up -d --wait                     # migrate re-sets the sc_app password; then the services
```

- **Secrets:** the restored cluster needs the same `secrets/` directory, or regenerated
  secrets (the owner and superuser passwords are set at init).
- **Check the result:** row counts match, and `SELECT * FROM aml.verificar_bitacora()`
  reports `valido = t`.
- **Restore drill:** run it quarterly on a scratch host, and record the duration.

### 5.10 Logs and monitoring

- **Container logs:** Docker's `local` driver, rotated at 20 MB × 5 per container
  (`docker compose logs <service>`).
- **nginx logs:** in `logs/nginx/` (access log with request id and TLS version), rotated
  by logrotate and read by fail2ban.
- **Ship everything off the host** to your SIEM with a log forwarder: container logs,
  nginx logs, auditd and the fail2ban log. Alert on:
  - `/salud` not 200;
  - container restarts;
  - fail2ban bans;
  - `ERROR` lines from the backend;
  - certificate expiry under 20 days;
  - backup job failures.

### 5.11 Egress to providers

The backend's `egress` network is internal by default, so the backend has no internet
access. When a provider integration needs outbound HTTPS, open only its API ranges:

1. **Name the bridge and open the network.** In `deploy/compose.env`:
   ```bash
   EGRESS_NETWORK_INTERNAL=false
   EGRESS_BRIDGE_NAME=sc-egress     # fixed host interface name for the firewall rules
   ```
2. **Restrict the destinations in Docker's `DOCKER-USER` chain**, before any traffic.
   Each `-I` inserts at the top, so the reject rule goes in first:
   ```bash
   sudo iptables -I DOCKER-USER -i sc-egress -j REJECT
   sudo iptables -I DOCKER-USER -i sc-egress -p tcp --dport 443 -d 203.0.113.0/24 -j RETURN   # SPEI provider
   sudo iptables -I DOCKER-USER -i sc-egress -p tcp --dport 443 -d 198.51.100.0/24 -j RETURN  # biometric provider
   ```
   - **Persist the rules** (`netfilter-persistent save`), and re-apply them whenever
     Docker recreates the chain.
   - **DNS is not affected.** Docker's embedded resolver queries from the host, not
     through the bridge.
3. **Recreate the networks:**
   ```bash
   sudo docker compose --env-file deploy/compose.env down
   sudo docker compose --env-file deploy/compose.env up -d --wait
   ```
4. **Verify.** A provider endpoint answers from the backend. Any other destination is
   refused.
5. **Tell the provider your source address.** That is the host's public address, which
   providers put on their allowlist.

If a provider cannot publish stable ranges, use an egress proxy with a domain allowlist
instead of opening 443 to everything.

---

## 6. Database migrations

**How they work:**

- **Embedded and immutable.** Migrations are SQL files in `migrations/`, embedded in the
  backend binary. Once applied they are immutable: sqlx stores each checksum in
  `_sqlx_migrations` and refuses to start if an applied file changed. Never edit an
  applied migration (0001–0009 are applied); fix forward with a new one.
- **Only the migrate job runs them.** That is the one-shot `synthetic-capital migrar`,
  the only holder of the owner credentials. The service runs with
  `MIGRAR_AL_INICIO=false` and `ARRANQUE_AL_INICIO=false`.

**Timeouts (set by the backend on every connection, migrate included):**

- **`lock_timeout = 5s`.** A migration that cannot get its lock within 5 s fails instead
  of queueing behind live traffic and stalling the bank. Retry in a quieter moment.
- **`statement_timeout = 30s`.** A migration that needs longer for one statement (an
  index build or backfill on a large table) must raise it for itself on its first line:
  `SET LOCAL statement_timeout = '30min';`. Each migration runs in its own transaction.
  A statement that cannot run inside a transaction, such as `CREATE INDEX CONCURRENTLY`,
  needs the sqlx `-- no-transaction` directive, and that migration file must contain
  only that statement.

**Procedure for every release that contains a migration:**

1. **Review.** Two reviewers, the SQL invariant suites green in CI, and a rehearsal on
   a staging copy of production-sized data with the duration recorded.
2. **Compatibility.** Prefer *expand → migrate → contract*: the new schema must work
   with both the old and the new binary, so the backend can roll forward or back
   without downtime. If a migration breaks the running binary (as 0009 did), drain the
   backend first (scale to zero) and plan a maintenance window.
3. **Backup point.** Take one right before migrating:
   - **AWS:** a manual RDS snapshot (point-in-time recovery also covers it);
   - **single host:** §5.9.
4. **Apply.**
   - **AWS:**
     1. Set `migrate_image_tag = <new>` and apply. Only the migrate task definition
        changes.
     2. Run the migrate task (§4, step 6) and check that its exit code is 0.
     3. Set `backend_image_tag = <new>`, remove `migrate_image_tag`, and apply.
   - **Single host:**
     ```bash
     # deploy/compose.env: BACKEND_IMAGE=<registry>/synthetic-capital-backend@sha256:<new>
     docker compose --env-file deploy/compose.env up -d --wait migrate backend
     ```
     This runs migrate with the new image first, and the backend only if it exits 0.
5. **Verify.**
   - `/salud` answers 200 and the backend log has no errors;
   - the AML worker runs (`target: "aml"` log lines);
   - `aml.verificar_bitacora()` reports valid;
   - the next reserve cut (corte) reconciles.
6. **Rollback.**
   - **Fix forward first:** a new migration.
   - **Restoring the backup point** loses every transaction since that point. It is a
     business decision that compliance and the treasury must approve, and it needs a
     reconciliation with SPEI and the custodian afterwards.

---

## 7. DNS and TLS

- **Records.**
  - **AWS:** an ALIAS record (Route 53) or a CNAME to `alb_dns_name`.
  - **Single host:** A (and AAAA only with IPv6 on the edge network, §5.2) records to
    the host.
  - **Both:** add a CAA record.
- **TLS 1.3 only** on both paths. Clients that cannot speak TLS 1.3 cannot connect; any
  change is a documented risk decision.
  - **AWS:** the ALB policy is `alb_ssl_policy`.
  - **Single host:** `nginx/snippets/tls.conf`. It offers the hybrid post-quantum key
    exchange `X25519MLKEM768`.
- **HSTS:** `max-age=63072000; includeSubDomains`. Once browsers have seen it, every
  subdomain must serve HTTPS for two years. `preload` is not set; submit to the preload
  list only after that decision.
- **Certificates.**
  - **AWS:** ACM issues and renews them automatically while the validation records
    exist.
  - **Single host:** certbot (§5.5).
  - **Monitor expiry either way.**
- **Unknown host names.** nginx aborts the TLS handshake for an unknown SNI and answers
  421 when the Host header differs from the SNI. It closes plain-HTTP connections for
  unknown hosts. Scanners walking IP ranges see neither the certificate nor the site.

---

## 8. Secrets and rotation

### 8.1 Inventory

| Secret | AWS | Single host (`secrets/`) | Read by | Suggested rotation |
|---|---|---|---|---|
| Schema owner password | RDS-managed secret | `db_owner_password`, `database_url_owner` | migrate only | RDS automatic / every 90 days |
| PostgreSQL superuser | — (RDS has none) | `postgres_password` | nobody (break-glass) | after any break-glass use; on staff change |
| `sc_app` password | `db-app/credentials`, `backend/DATABASE_URL` | `db_app_password`, `database_url` | backend; migrate sets it | 90 days |
| `JWT_SECRET` | Secrets Manager | `jwt_secret` | backend | 90 days (signs every session out) |
| SPEI / KYC webhook HMAC keys | Secrets Manager (+ `*_ANTERIOR`) | `*_webhook_secret` (+ `*_previous`) | backend; shared with the provider | per contract, at least yearly |
| `MFA_CLAVE_CIFRADO` (encrypts the staff's TOTP secrets) | Secrets Manager (+ `MFA_CLAVE_CIFRADO_ANTERIOR` during a rotation) | `mfa_encryption_key` (+ `_previous`) | backend; migrate during a rotation | yearly, or at once if exposed |
| Bootstrap operator password | Secrets Manager | `bootstrap_operador_password` | migrate (first creation only) | hand over; the operator enrolls two-step verification at first sign-in and changes the password with the reset flow |

**Never** put a secret in `deploy/compose.env`, `terraform.tfvars`, a CI variable, a
ticket or chat. On AWS, populate secrets as in §4 step 5.

### 8.2 Rotation on a single host (tested)

| Rotate | Command | Then |
|---|---|---|
| JWT | `sudo scripts/generate-secrets.sh --rotate jwt` | `docker compose ... up -d --force-recreate backend` |
| sc_app | `--rotate app` | in a quiet window: `up -d --force-recreate migrate backend` (migrate sets the new SCRAM verifier) |
| Webhook keys | `--rotate spei` (or `kyc`) | recreate the backend, then hand the new key to the provider. The old key moves to `*_previous` and stays accepted. Once the provider signs with the new key: `--clear-previous spei` and recreate the backend |
| Schema owner | `--rotate owner` | nothing else: the script sets the new password in the running cluster first, then rewrites the files |
| Superuser | `--rotate superuser` | as above |
| MFA key | `--rotate mfa` | `up -d --force-recreate migrate backend`: migrate re-encrypts every TOTP secret with the new key (its log says how many) while the backend accepts both. Only then `--clear-previous mfa` and recreate the backend. Clearing earlier locks out staff |

Always pass `--env-file deploy/compose.env` to the script as well.

After a rotation, a running container still shows the old secret file. That is how
Docker bind mounts work, and it is why nothing reads secrets after start-up and why
`scripts/db-client.sh` reads the current password on the host.

### 8.3 Rotation on AWS

See `terraform/README.md`, "Secrets": JWT (put a new value, then force a new
deployment); webhook keys (enable `*_ANTERIOR`, then swap); `sc_app`
(`terraform apply -replace=random_password.db_app`, run migrate at once, then redeploy
in a quiet window); the owner password (RDS rotates it); `MFA_CLAVE_CIFRADO` (five ordered
steps with `enable_previous_mfa_key`: the migrate task re-encrypts the TOTP secrets).

---

## 9. Production launch checklist

Each item needs evidence attached to the launch ticket (an output, a screenshot or a
link).

**Infrastructure**
- [ ] AWS account baseline in place: CloudTrail, GuardDuty, Config, Security Hub,
      region SCPs, AWS Backup with vault lock, log archive.
- [ ] Terraform state bucket hardened. `terraform plan` shows no drift.
- [ ] Two to three availability zones pinned. Multi-AZ RDS. Deletion protection on.
- [ ] Alarms wired to the on-call rota: ALB 5xx and target health, RDS CPU, storage and
      connections, proxy borrow timeouts, WAF blocks, ECS task failures.
- [ ] Restore drill done (RDS point-in-time restore to a scratch instance; single host:
      §5.9). Duration recorded.

**Security**
- [ ] Every CI gate green on the release commit (§3). No unexpired exception without an
      approver.
- [ ] `MODO_DEMO=false`: no `MODO_DEMO activo` warning in the backend log.
- [ ] Backend log shows `usuario=sc_app`, not the owner.
- [ ] Database unreachable from outside (§5.7 / security groups reviewed).
- [ ] TLS: TLS 1.2 refused; HSTS, CSP, X-Frame-Options and nosniff present.
- [ ] Provider allowlists set: `backend_egress_cidrs`, the webhook CIDRs (WAF, or the
      nginx `allowlist/`).
- [ ] Every secret generated fresh for production. Nothing reused from staging.
- [ ] External penetration test of the deployed environment, with findings closed or
      accepted.
- [ ] **Launch blockers in §12 resolved or formally accepted** (DBA access path on AWS;
      an email provider for password resets).
- [ ] Every staff account enrolled two-step verification: one `MFA_ACTIVADO` row per staff
      member in `kyc.eventos_autenticacion`, and no staff row with `mfa_enabled = false`.

**Compliance and operations**
- [ ] At least two `CUMPLIMIENTO` users and one `AUDITOR` provisioned (§10.3), with
      the ticket and the second approver recorded.
- [ ] Bootstrap operator password handed over and the hand-over recorded; the operator
      enrolled two-step verification at the first sign-in.
- [ ] Provider contracts signed. Webhook keys exchanged through the agreed channel.
- [ ] CLABE prefix, reserve account and SOFOM cash account set to the real values
      (`app_environment` / `deploy/compose.env`).
- [ ] Data residency confirmed: region `mx-central-1`, backups in-region.
- [ ] Runbooks for this document's procedures in the on-call wiki. Contacts for the
      providers and the custodian.
- [ ] Go/no-go signed by the release manager, security, compliance and the business
      owner.

---

## 10. Operations

### 10.1 Health endpoints

| Endpoint | Meaning | Used by |
|---|---|---|
| `GET /vivo` (internal only) | process alive; no database | container healthcheck (`synthetic-capital salud vivo`) |
| `GET /salud` | ready: answers 200 only when the database answers | ALB target group, uptime monitoring, nginx clients |
| `GET /nginx-salud` (127.0.0.1 only) | nginx alive | nginx container healthcheck |

A database outage turns `/salud` into 503 without restarting the backend, because the
liveness check never touches the database. nginx answers API clients with the JSON
`503 REINTENTAR`, which the frontend translates.

### 10.2 Releases without migrations

- **AWS:** set `backend_image_tag` / `frontend_image_tag` and apply. ECS rolls the
  services with a deployment circuit breaker and automatic rollback.
- **Single host:** set `BACKEND_IMAGE` / `FRONTEND_IMAGE`, then
  `docker compose --env-file deploy/compose.env up -d --wait`. Expect a second or two of
  503 while each container is recreated.

### 10.3 Provisioning internal staff

Internal users are provisioned by the schema owner, never by an environment variable in
production.

1. **The person registers.** They sign up in the app with their institutional email and
   a password they choose.
2. **The DBA promotes the account.** A second person approves the ticket first.
   ```bash
   sudo scripts/db-client.sh -- -v ON_ERROR_STOP=1 -c \
     "UPDATE kyc.credenciales SET rol = 'CUMPLIMIENTO'
       WHERE cliente_id = (SELECT id FROM kyc.clientes WHERE email = 'oficial@bank.example.mx')"
   ```
   The roles are `OPERADOR`, `AUDITOR` and `CUMPLIMIENTO`. The statement must report
   `UPDATE 1`.
3. **Record it.** Attach the statement and its output to the ticket.
4. **Two-step verification.** The next sign-in asks the person to set up an
   authenticator app (QR code and first code) before anything else: staff never get a
   session from a password alone. Check the enrollment:
   ```bash
   sudo scripts/db-client.sh -- -c "SELECT evento, ip, ocurrido_en FROM kyc.eventos_autenticacion
     WHERE cliente_id = (SELECT id FROM kyc.clientes WHERE email = 'oficial@bank.example.mx') ORDER BY id"
   ```
   Enroll right after the promotion: whoever holds the password first binds the
   authenticator.

- **AWS:** this needs the DBA access path listed in §12.
- **Revoking access:** set the role back to `CLIENTE` and revoke the sessions in the
  same statement (`tokens_validos_desde = now()`). Staff sessions last at most
  `JWT_TTL_PERSONAL_MINUTOS` (default 30).
- **Lost or replaced phone:** after verifying the person's identity out of band (and
  with a second approver), the DBA resets the second factor. The change is recorded as
  `MFA_DESACTIVADO`, and the next sign-in enrolls again:
  ```bash
  sudo scripts/db-client.sh -- -v ON_ERROR_STOP=1 -c \
    "UPDATE kyc.credenciales SET mfa_enabled = FALSE, totp_secret = NULL, totp_ultimo_paso = NULL,
            tokens_validos_desde = now()
      WHERE cliente_id = (SELECT id FROM kyc.clientes WHERE email = 'oficial@bank.example.mx')"
  ```
  The application itself cannot do this: with the second factor on, only the schema
  owner may turn it off or change its secret (migration 0010).
- **Forgotten password:** the person uses "Forgot your password?". The link works once,
  expires in 30 minutes and revokes every session; two-step verification stays on.
  Until an email provider is connected (§12), production sends no link; support
  resets the password instead.

### 10.4 Break-glass database access

- **Single host:** `sudo scripts/db-client.sh --superuser` opens an interactive psql as
  `postgres`. Use it only under an incident ticket, and rotate it afterwards
  (`generate-secrets.sh --rotate superuser`). Routine work uses the owner (without
  `--superuser`).
- **AWS:** there is no standing path (no bastion, ECS Exec disabled). See §12.

### 10.5 Incidents worth a runbook

| Symptom | First checks |
|---|---|
| `/salud` 503 | database up? (`docker compose ps db`, RDS events); backend log; connection limits (`DB_MAX_CONEXIONES`, proxy borrow timeouts) |
| 429 for legitimate users | per-IP limits (`nginx/snippets/rate-limits.conf`, WAF rate rules); a carrier-grade NAT shares one address |
| Webhook deliveries refused (401/403) | HMAC key mismatch (rotation in progress? `*_previous` / `*_ANTERIOR`); source allowlist |
| migrate exits non-zero | its log: lock timeout (retry later), statement timeout (§6), checksum mismatch (an applied migration was edited: never deploy it) |
| Certificate near expiry | `certbot renew --dry-run`; the deploy-hook output; ACM validation records |

---

## 11. Compliance mapping

This table shows where each control is implemented and where the evidence is. It is an
aid for the compliance team and the auditors, not a certification.

| Control area | CNBV (fintech and AML/CFT provisions) | ISO/IEC 27001:2022 Annex A | SOC 2 (TSC 2017) | Implementation | Evidence |
|---|---|---|---|---|---|
| Network segregation | information-security requirements | 8.20, 8.21, 8.22 | CC6.6 | private data tier; internal Docker networks; security groups; WAF | Terraform; `docker-compose.yml`; §5.7 |
| Least privilege, privileged access | access control | 5.15, 5.18, 8.2 | CC6.1, CC6.3 | `sc_app` NOINHERIT; non-superuser owner; superuser break-glass only; no task role | `pg_authid`; Terraform IAM |
| Segregation of duties | internal control | 5.3 | CC5.1 | role matrix (§2.1); four-eyes in the AML console (database-enforced) | AML suite; tickets |
| Authentication secrets | information security | 5.17 | CC6.1 | Secrets Manager / Docker secrets; SCRAM-SHA-256 everywhere; rotation (§8) | §8; rotation records |
| Cryptography | information security | 8.24 | CC6.1, CC6.7 | TLS 1.3 at the edge; TLS to RDS (`verify-full` to the proxy); KMS CMKs | ALB/nginx config; KMS key policies |
| Logging and monitoring | operation records; AML audit trail | 8.15, 8.16 | CC7.2, CC7.3 | ALB/WAF/flow logs; container logs; fail2ban; tamper-evident AML audit log (`aml.verificar_bitacora`) | log groups; SIEM |
| Vulnerability management | information security | 8.8 | CC7.1 | Trivy gates (images, lockfiles, IaC, third-party images); Dependabot; digest pins | CI history; `.github/trivyignore.yaml` |
| Secure development and change | change management | 8.25, 8.28, 8.29, 8.32 | CC8.1 | protected `main`; required checks; migration procedure (§6) | PRs; CI; release tickets |
| Backup and recovery | business continuity | 8.13, 8.14 | A1.2, A1.3 | RDS 35-day point-in-time recovery, Multi-AZ; tested dump and restore (§5.9) | restore drill records |
| Configuration hardening | information security | 8.9 | CC6.8, CC7.1 | `hardening.sh`; read-only, non-root containers; distroless images | §5.2 output; `docker inspect` |
| Data residency and privacy | data residency; personal data (LFPDPPP) | 5.34 | P-series, C1.1 | region `mx-central-1`; no cross-region replication | Terraform; AWS Config |

The AML/CFT controls themselves (risk matrix, monitoring, UIF reports, four-eyes) are
documented in `README.md`, "AML/CFT monitoring". The compliance team owns the mapping
to the specific provisions.

---

## 12. Residual risks and open items

| # | Item | Impact | Mitigation now | Next step (owner) |
|---|---|---|---|---|
| 1 | **Password-reset email is simulated** (no email provider integrated): outside `MODO_DEMO` the link is neither sent nor logged | people who forget their password need support | support resets passwords through the DBA; the flow itself (hashed single-use token, 30 minutes, every session revoked, same answer for unknown emails) is implemented and tested | connect an email provider in the residency region to `backend/src/auth/correo.rs` (backend) — **launch blocker** |
| 2 | Two-step verification is TOTP only: no recovery codes, no WebAuthn/passkeys | a lost phone needs a DBA reset (identity check, second approver); a TOTP code can be phished in real time | replay protection, account lock after 5 failed codes or passwords, staff sessions of at most 30 minutes, every enrollment, code and reset in the immutable audit table | recovery codes and WebAuthn for staff (product, security) |
| 3 | The first enrollment trusts whoever holds the password | someone with a new staff member's initial password could bind their own authenticator | promotion and enrollment in the same ticket (§10.3); the audit table records each enrollment's IP | deliver initial credentials out of band; check the `MFA_ACTIVADO` event |
| 4 | **No DBA access path on AWS** (no bastion, ECS Exec disabled) | staff provisioning (§10.3) and break-glass need one | none | add a short-lived, audited path: for example a one-off ECS task with a PostgreSQL client image in the migrate security group, the owner secret injected, the reviewed SQL passed in the run-task overrides (recorded in CloudTrail) (infrastructure) — **launch blocker** |
| 5 | Single-host topology: no high availability; recovery point = backup interval | an outage, or data loss since the last dump | encrypted daily dumps, off-site copies, tested restore | use AWS for production; WAL archiving (e.g. pgBackRest) if the single host must carry production |
| 6 | Single host: once egress is enabled, its restriction is a host firewall rule set (§5.11), not code | a mistake in the rules opens more than the providers | rules reviewed in the change ticket; default stays no egress | manage the rules with configuration management |
| 7 | Single host: `sslmode=disable` between backend and database | traffic in clear on the host's internal bridge | internal network, same host, no other tenants | TLS in the PostgreSQL container if the host is shared |
| 8 | Fargate cannot set `no-new-privileges` | — | non-root, no capabilities, distroless (nothing to escalate to) | none (platform limit) |
| 9 | The ALB cannot add Referrer-Policy, Permissions-Policy, COOP or CORP | headers missing on API responses on AWS | pages get the first two from `next.config.ts` | add them in the backend (backend) |
| 10 | RDS Proxy pins every backend connection (session `SET`, prepared statements, `SET LOCAL ROLE`) | no multiplexing | the proxy still gives failover, a connection cap and a public-CA certificate | size `backend_max_tasks × backend_db_max_connections` under the proxy cap |
| 11 | Migrations run with `statement_timeout = 30s` | a long migration statement fails | per-migration `SET LOCAL` (§6) | none |
| 12 | Third-party image findings: `gosu` (Go stdlib) in postgres; curl and brotli in nginx | none reachable: gosu never runs (db starts as uid 70); nginx never invokes curl | path-scoped, dated exception for the CRITICAL gosu finding; weekly scan | take Dependabot's digest bumps (infrastructure) |
| 13 | Single host: nginx uid 101 maps to a host account | that account can read the nginx logs and TLS key | warning from `hardening.sh` | `--nginx-uid` with an unused uid (§5.2) |
| 14 | Single host: buffered KYC uploads (up to 13 MB) use nginx's 128 MB `/tmp` tmpfs | many simultaneous uploads could exhaust it (413 or 500) | 2 concurrent uploads per address | raise the tmpfs and the container memory with traffic |
| 15 | Bootstrap operator password is a generated secret file | must be handed over securely | 0400 root-readable file; the operator must enroll two-step verification at the first sign-in | change it through the reset flow once item 1 is resolved |
| 16 | WAF blocks Tor and anonymising proxies (`waf_block_anonymous_ips`) | some legitimate users cannot connect | — | business confirmation |
| 17 | The CSP allows `'unsafe-inline'` scripts (Next.js inlines its bootstrap scripts), and the session JWT is in `sessionStorage` | an XSS bug would expose the session | React escapes all output; the frontend has no raw-HTML sinks (`dangerouslySetInnerHTML`, `innerHTML`, `eval`); `frame-ancestors 'none'`, `object-src 'none'`, `base-uri 'none'`; the JWT is short-lived and cleared when the tab closes | nonce-based CSP through Next.js middleware; HttpOnly, SameSite session cookies (frontend, backend) |
| 18 | Images are not signed and carry no SBOM attestation | registry tampering is not detected at deploy time | digest pinning end to end; ECR tags are immutable | cosign signatures and an SBOM (Trivy or Syft) in the release pipeline, verified before deploy (infrastructure) |

---

## Appendix A. Configuration reference

- **Backend variables:** `.env.example`. Every variable `NAME` also accepts
  `NAME_FILE`. README, "Production database users", explains the container contract.
- **Single host:** `deploy/compose.env.example` holds the non-secret settings, and
  `scripts/generate-secrets.sh --help` explains the secrets.
- **AWS:** `terraform/variables.tf`, documented in `terraform/README.md` (Inputs), with
  an example in `terraform/terraform.tfvars.example`.
- **Edge:** `nginx/` (rate-limit table in `nginx/snippets/rate-limits.conf`; webhook
  allowlists in `nginx/allowlist/`).

## Appendix B. Version pins outside Dependabot

Dependabot updates the npm lockfiles, Cargo, GitHub Actions, the Dockerfile and compose
base images (digests included) and the Terraform providers. These pins are bumped by
hand:

| Pin | Where |
|---|---|
| Rust toolchain `1.97` | `ci.yml` (`RUST_TOOLCHAIN`); keep in step with the `rust:1.97.x` builder in `backend/Dockerfile` |
| Node major `22` | `ci.yml` (`NODE_VERSION`); runtime `nodejs22` base |
| Trivy, Terraform, tflint, hadolint, ShellCheck, actionlint versions and checksums | `ci.yml` / `security.yml` `env:` |
| tflint AWS ruleset | `.github/linters/.tflint.hcl` |
| PostgreSQL major (16) | `docker-compose*.yml` tag, `db_engine_version` in Terraform |
| RDS CA (`rds-ca-rsa2048-g1`) | `terraform/rds.tf` |

## Appendix C. Verification evidence

Gathered while preparing this document, in a sandbox with Docker 29.8 and compose 5.6.

**Single-host stack** (`docker-compose.yml`, production-mode secrets owned by root):

- **Start-up:** a fresh volume came up in about 12 s. migrate exited 0. db, backend,
  frontend and nginx were healthy.
- **Containers:** db ran as 70:70, backend and frontend as 65532:65532, nginx as
  101:101. Every container had `ReadonlyRootfs=true`, `CapDrop=[ALL]`,
  `no-new-privileges`, and memory, CPU and PID limits. Only nginx published ports.
- **Isolation:** nginx and the frontend could neither resolve nor reach the database.
  The frontend could neither resolve nor reach the backend, by name or by address. The
  frontend, the database and the backend's egress network (internal by default) had no
  internet access. nginx reached the frontend and the backend. The backend reached the
  database.
- **Database roles:**
  - `postgres` is the only superuser.
  - `synthetic_owner` has LOGIN and CREATEROLE but not SUPERUSER, CREATEDB or BYPASSRLS,
    and owns the database and every schema.
  - `sc_app` is NOINHERIT and holds 8 memberships with `INHERIT FALSE, SET TRUE` (9
    since migration `0010`, below).
  - Every password is stored as a SCRAM-SHA-256 verifier, and the backend connects as
    `sc_app`.
- **Secrets:** no secret value appears in `docker inspect` or in the image history.
- **Edge:**
  - TLS 1.3 with HTTP/2; TLS 1.2 refused; the handshake refused for an unknown SNI.
  - HTTP is redirected to HTTPS with a 301.
  - HSTS, CSP, X-Frame-Options DENY, nosniff, Referrer-Policy and Permissions-Policy
    are present.
- **Browser end-to-end tests through the TLS edge:** AML suite 113/113, smoke suite
  47/47.
- **Rotations:**
  - Owner and superuser passwords changed in the database; migrate and superuser logins
    worked with the new values, and the old superuser password was refused.
  - `app`, `jwt` and `spei` recreated the backend healthy, and the old webhook secret
    moved to `_previous`.
  - `--clear-previous` emptied it.
- **Backup and restore** into a fresh volume: `pg_restore --exit-on-error` exited 0.
  migrate exited 0 and the backend was ready. The migrations and every non-empty
  table's row count were identical. `aml.verificar_bitacora()` reported `valido = t`
  (105 rows, 2 seals) on both databases.
- **Certificate hook:** a certbot-style renewal swapped the served certificate
  (fingerprint changed) without restarting nginx.

**Two-step verification and password reset** (migration `0010`): the same single-host
stack, both images rebuilt from this change, production mode (`MODO_DEMO=false`), every
request through the TLS edge:

- **Start-up:** from a fresh volume, migrate exited 0 and every service was healthy.
  `sc_app` holds 9 memberships with `INHERIT FALSE, SET TRUE` (`sc_auth` added) and
  cannot read `totp_secret`.
- **Enrollment of the bootstrap operator:**
  - The password alone answered `MFA_ENROLLMENT_REQUIRED`, and that token was refused
    on `/api/v1/aml/*`.
  - Setup returned the secret and an SVG QR code with `Cache-Control: no-store`.
  - The database held only a `v1:` envelope. It decrypts with `mfa_encryption_key`
    under this account's associated data, and not under another account's.
  - A wrong first code was refused. The first valid code enabled MFA and started the
    session, and the enrollment token stopped working.
- **Sign-in:** `2FA_REQUIRED`, then `verify-mfa`. A code already used was refused. The
  access token carried `mfa: true` and lasted 30 minutes.
- **Password reset:** the same 202 for an account and for an unknown email. Only the
  token's SHA-256 was stored, and neither the link nor the token reached the logs.
- **Audit:** `kyc.eventos_autenticacion` recorded the secret's generation, the rejected
  code, the activation, the verification and the reset request.
- **Key rotation** (§8.2): `--rotate mfa`, then recreating migrate and the backend,
  re-encrypted the secret (`recifrados=1`). Sign-in worked with both keys loaded, and
  again after `--clear-previous mfa`.
- **Pages:** `/auth/login`, `/auth/mfa-setup`, `/auth/forgot-password` and
  `/auth/reset-password` answered 200 through nginx. The CSP allows the `data:` QR image.
- **Browser end-to-end tests** (local stack, `e2e/aml-stack.sh`): AML suite 121/121, now
  enrolling every staff account at its first sign-in and retrying after a wrong code;
  smoke suite 47/47.

**Images and scans:**

- **Images:** both rebuilt on Debian 13 bases, backend 58 MB and frontend 269 MB.
- **Trivy, fixable HIGH or CRITICAL:** 0 in both images. Before the move, the debian12
  nodejs22 base carried the CRITICAL libssl3 CVE-2026-31789 plus six HIGH OpenSSL
  findings, and the cc base one HIGH.
- **CRITICAL gates** with the trivyignore file: pass for postgres and nginx.

**Code and infrastructure checks** (the same tools and settings as CI, re-run with
migration `0010`):

- **Backend:** rustfmt and clippy `-D warnings` clean; 294 tests passed, 0 failed,
  among them the 11 of `tests/mfa.rs`.
- **SQL invariant suites:** all six passed on freshly migrated databases
  (`0010_auth_mfa`: 32 checks).
- **Frontend:** lint and typecheck clean; 83 unit tests passed; `next build` succeeded.
- **Terraform:** fmt, init and validate pass, with the lock file unchanged. tflint is
  clean. checkov: 388 passed, 0 failed, 16 skipped (each skip justified inline). That
  is two fewer of each than before `0010`, with no check lost: checkov no longer expands
  the `for_each` of `aws_secretsmanager_secret.backend` (its `merge` now holds two
  conditional maps), so it reports CKV_AWS_149 (passed) and CKV2_AWS_57 (skipped) once
  for the resource instead of once per secret.
- **Trivy:** 0 HIGH or CRITICAL misconfigurations; the secret scan of the tracked tree
  is clean.

**Static checks:**

- ShellCheck is clean on every script.
- actionlint is clean.
- hadolint is clean on both Dockerfiles.
