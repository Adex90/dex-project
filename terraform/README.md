# Synthetic Capital on AWS (Terraform)

Reference architecture for production: one flat root module, Terraform >= 1.10, AWS
provider 6.x. It implements the infrastructure contract of the production brief; the
step-by-step runbook lives in DEPLOYMENT.md.

```
internet -> WAF -> ALB (public subnets, TLS 1.3, HTTP -> HTTPS)
                    |- /api/*, /salud -> backend  (ECS Fargate, app subnets, :8080)
                    `- everything else -> frontend (ECS Fargate, app subnets, :3000)
backend  -> RDS Proxy (TLS) -> RDS PostgreSQL 16 Multi-AZ   (data subnets: no route out)
migrate  -> RDS instance directly, as the schema owner      (one-off ECS task)
DBA      -> Session Manager port forwarding -> bastion (app subnet, no inbound) -> RDS instance
```

| File | Contents |
|---|---|
| `versions.tf`, `providers.tf` | version pins, S3 backend (partial config), `allowed_account_ids`, default tags |
| `variables.tf` | every input, with validation |
| `main.tf` | data sources, names, tags, image references |
| `vpc.tf` | VPC, 3 tiers x 2-3 zones, NAT per zone, VPC endpoints, flow logs, data-subnet NACL |
| `security_groups.tf` | the security group matrix (documented at the top of the file) |
| `kms.tf` | CMKs with yearly rotation: rds, secrets, logs, ecr, ebs |
| `logs.tf` | KMS-encrypted log groups, S3 bucket for ALB access/connection logs |
| `secrets.tf` | Secrets Manager secrets (see "Secrets") |
| `rds.tf` | parameter group, Multi-AZ instance, RDS Proxy |
| `ecr.tf`, `ecs.tf` | repositories; cluster, task definitions (backend, frontend, migrate), services, autoscaling |
| `alb.tf`, `waf.tf` | load balancer, certificate, DNS; web ACL |
| `bastion.tf` | DBA access path: Session Manager-only bastion, its security group rules, the `dba-tunnel` IAM policy (see "DBA access") |
| `outputs.tf` | endpoints, secret ARNs, the migrate `run-task` command, `dba_tunnel` |

## Inputs

Required: `region` (no default: data residency, normally `mx-central-1`),
`aws_account_id`, `domain_name`, `backend_image_tag`, `frontend_image_tag`.
Recommended: `availability_zones` (pin them), `acm_certificate_arn` or `route53_zone_id`,
`backend_egress_cidrs` (provider API ranges; empty = no internet egress),
`spei_webhook_allowed_cidrs` / `kyc_webhook_allowed_cidrs`, `app_environment`
(`CLABE_PREFIJO`, `RESERVA_*`, `CAJA_*`, ...), `bootstrap_operator_email`, `tags`.
See `terraform.tfvars.example`. No secret is ever a variable.

## Deploying (summary)

1. Account baseline and state bucket in place (below). `cp backend.hcl.example backend.hcl`,
   edit, `terraform init -backend-config=backend.hcl`.
2. First apply with `-var backend_min_tasks=0 -var frontend_min_tasks=0`. Image tags must
   be set; the images need not exist yet. Without `route53_zone_id`, first run
   `terraform apply -target=aws_acm_certificate.this`, create the records from
   `terraform output acm_validation_records` at the DNS provider, then apply (it waits
   for validation), and point `domain_name` at `alb_dns_name`.
3. Push the images to `ecr_repository_urls` (immutable tags; the frontend built with
   `NEXT_PUBLIC_API_URL=https://<domain_name>`).
4. Populate the secrets listed in `secrets_to_populate`.
5. Run the migrate task: `TASK=$(eval "$(terraform output -raw migrate_run_task_command)")`,
   then `aws ecs wait tasks-stopped --cluster <cluster> --tasks "$TASK"`; the exit code
   (`aws ecs describe-tasks ... --query 'tasks[0].containers[0].exitCode'`) must be 0.
   Logs: `/ecs/<project>-<env>/migrate`.
6. Apply again without the `*_min_tasks=0` overrides; autoscaling raises the services to
   their minimum.
7. Attach `dba_tunnel_policy_arn` to the DBAs (and nobody else), hand them
   `terraform output -json dba_tunnel`, and create the first compliance officers through
   `scripts/db-tunnel.sh` (DEPLOYMENT.md §4.1).

Releases with migrations: set `migrate_image_tag` to the new tag and apply (only the
migrate task definition changes), run the migrate task, then move `backend_image_tag`
and drop `migrate_image_tag`. Migrations that break the previous binary (like 0009)
need the backend drained first.

## Secrets

| Secret | Value | In state? |
|---|---|---|
| `<name>/db-app/credentials` | `{"username":"sc_app","password":...}`: RDS Proxy auth, `DB_APP_PASSWORD` of the migrate task | yes (generated) |
| `<name>/backend/DATABASE_URL` | `postgres://sc_app:...@<proxy>:5432/<db>?sslmode=verify-full` | yes (generated) |
| `<name>/backend/JWT_SECRET`, `SPEI_WEBHOOK_SECRETO`, `KYC_WEBHOOK_SECRETO` (+ `*_ANTERIOR` with `enable_previous_webhook_secrets`) | put out of band | no |
| `<name>/backend/MFA_CLAVE_CIFRADO` (+ `MFA_CLAVE_CIFRADO_ANTERIOR` with `enable_previous_mfa_key`): AES-256-GCM key of the staff's TOTP secrets, 64 hex characters (`openssl rand -hex 32`) | put out of band | no |
| `<name>/migrate/BOOTSTRAP_OPERADOR_PASSWORD` (with `bootstrap_operator_email`) | put out of band | no |
| RDS-managed master secret (`db_master_user_secret_arn`) | generated and rotated by RDS | no |

Populate without putting values in shell history or process arguments:

```bash
openssl rand -hex 64 | tr -d '\n' | aws secretsmanager put-secret-value \
  --secret-id "$(terraform output -json secrets_to_populate | jq -r .JWT_SECRET)" \
  --secret-string file:///dev/stdin
```

The webhook keys (>= 32 random characters) are shared with the providers. ECS refuses to
start a task whose secret has no value, which is why the services start at zero tasks.

Rotation. `JWT_SECRET`: put a new value, `aws ecs update-service --force-new-deployment`
(open sessions end). Webhook keys: enable `*_ANTERIOR`, copy the current key there, put
the new key, redeploy, hand it to the provider, then disable `*_ANTERIOR`. The `sc_app`
password: `terraform apply -replace=random_password.db_app`, run the migrate task at once
(it sets the new SCRAM verifier), then force a new backend deployment; until the
redeploy, old tasks cannot open new connections through the proxy, so do it in a quiet
window. The owner password is rotated by RDS; the migrate task reads it at start.
`MFA_CLAVE_CIFRADO` (the staff's TOTP secrets are stored encrypted with it, never in clear):
1. `terraform apply -var enable_previous_mfa_key=true -target='aws_secretsmanager_secret.backend["MFA_CLAVE_CIFRADO_ANTERIOR"]'`
   creates the empty secret (ECS cannot start a task whose secret is empty, so nothing deploys yet).
2. Put the current key in `MFA_CLAVE_CIFRADO_ANTERIOR`, then a new one (`openssl rand -hex 32`)
   in `MFA_CLAVE_CIFRADO`, and at once:
3. `terraform apply -var enable_previous_mfa_key=true`: the backend redeploys decrypting with
   either key, and the migrate task gets both.
4. Run the migrate task: it re-encrypts every TOTP secret with the new key (its log says how many).
5. `terraform apply` (the flag back to `false`): the backend redeploys with the new key only.
Never drop the previous key before step 4: staff whose secret it still encrypts could not sign in.

## Database access and TLS

- The service holds only `DATABASE_URL` (`sc_app`, via RDS Proxy, `verify-full`). The
  proxy certificate is issued by ACM under Amazon Root CA 1, which the backend's sqlx
  (rustls + webpki roots) trusts.
- The migrate task holds the owner credentials, which nothing else can read (separate
  execution role) except DBAs holding the `dba-tunnel` policy ("DBA access"). RDS
  rotates the owner password, so it cannot be embedded in a URL:
  `DATABASE_URL_PROPIETARIO` has no password and ECS injects `PGPASSWORD` from the
  RDS-managed secret; sqlx fills a password-less URL from `PGPASSWORD`. The instance
  certificate comes from the private RDS CA (`rds-ca-rsa2048-g1`), so the regional RDS
  bundle is passed inline in `PGSSLROOTCERT` (sqlx accepts PEM there and adds it to its
  roots). The bundle is downloaded at plan time from truststore.pki.rds.amazonaws.com;
  set `rds_ca_bundle_pem` where plans have no internet access. This contract was tested
  against the backend image with a TLS + SCRAM-only PostgreSQL 16: without the bundle
  `verify-full` fails (`UnknownIssuer`); with it, `migrar` succeeds and is idempotent.
- Parameter group: `rds.force_ssl=1`, SCRAM only (`password_encryption=scram-sha-256`,
  `rds.accepted_password_auth_method=scram`), TLS >= 1.2, connection, DDL, slow-statement
  and lock logging, bind values never logged. No server-wide `statement_timeout` or
  `lock_timeout`: they would cut long migrations; the backend sets its own per session.

**RDS Proxy pinning caveat.** The backend's sqlx pool runs session-level `SET`
statements when it connects, uses named prepared statements and opens every transaction
with `SET LOCAL ROLE`. Each of these pins the client connection to one database
connection, so every backend connection is pinned for its lifetime and the proxy does no
transaction-level multiplexing. It still gives faster, DNS-free failover, a cap on
database connections (`db_proxy_max_connections_percent`, excess clients wait up to the
borrow timeout) and TLS with a public-CA certificate. Size it as
`backend_max_tasks x backend_db_max_connections < max_connections x percent / 100`.

## DBA access

`bastion.tf` gives DBAs a path to the RDS instance without opening a port: Session
Manager port forwarding through a bastion that nothing can reach. DEPLOYMENT.md §4.1 is
the runbook; `scripts/db-tunnel.sh` opens the tunnel.

- **The bastion.** Amazon Linux 2023 (standard image, which ships the SSM agent) on
  `dba_bastion_instance_type` (default `t4g.nano`; the AMI follows the type's
  architecture), in an app subnet (`dba_bastion_availability_zone`, default the first
  zone). It has no public address and no key pair, IMDSv2 only, sshd disabled, and a
  KMS-encrypted root volume (`ebs` key). Its role holds only
  `AmazonSSMManagedInstanceCore`.
- **Network.** Its security group has no ingress rule. Egress: 5432 to the database
  group and 443 to the endpoint group. The database group admits 5432 from it. The
  `ssmmessages` and `ec2messages` interface endpoints (with `ssm`, already there) carry
  the agent's channel, so the bastion never needs the internet. It sits in an app subnet,
  whose CIDR the data-subnet NACL already admits on 5432.
- **Who.** The `dba-tunnel` IAM policy (output `dba_tunnel_policy_arn`) allows
  `ssm:StartSession` on this instance with `AWS-StartPortForwardingSessionToRemoteHost`
  only (`ssm:SessionDocumentAccessCheck` refuses shells). It also allows reading the
  master user secret (decrypted through Secrets Manager only) and checking that the
  bastion is online. Terraform attaches it to nobody: the infrastructure administrator
  attaches it to the DBA's IAM Identity Center permission set or role.
- **What the script needs.** Output `dba_tunnel` (no secret): region, instance, document,
  RDS endpoint and port, database, master user, its secret ARN, the RDS CA bundle URL.
  `null` when the bastion is off.
- **Patching.** The instance is replaced from the latest AMI on any apply after AWS
  publishes one (`create_before_destroy`). It holds no state.
- **Off switch.** `dba_bastion_enabled = false` removes the instance, its endpoints, its
  rules and the policy. The `ebs` key stays, so the switch never schedules a key deletion.
- **Session Manager preferences** are account-wide and not managed here. If they encrypt
  session data with KMS, set `dba_session_kms_key_arn`: the instance may then decrypt and
  DBAs may generate data keys with it.

## Edge

- ALB: TLS policy `alb_ssl_policy` (TLS 1.3 only by default), HSTS/CSP/nosniff/DENY
  added by the listener (the same values as the nginx edge; the ALB cannot add
  Referrer-Policy, Permissions-Policy, COOP or CORP, so pages get the first two from
  `next.config.ts` and API answers get none), `Server` header removed, invalid headers dropped, strictest
  desync mitigation, X-Forwarded-For appended (the backend runs with
  `HTTP_PROXY_CONFIABLE=true`). Idle timeout (90 s) exceeds the backend's request
  deadline, and the backend's idle keep-alive limit (`HTTP_CABECERAS_SEG`) is set 15 s
  above it to avoid 502s. The ALB does not buffer bodies like nginx, so
  `HTTP_CUERPO_SEG`/`HTTP_PETICION_SEG` are raised for slow 12 MB KYC uploads.
- WAF: IP reputation and anonymous-IP lists, common rule set, known bad inputs, SQLi;
  per-IP rate limits for `/api/v1/auth/`, the KYC upload, the webhooks and the rest of
  `/api/`; `/_next/image` returns 404 (unused). Behind an ALB, WAF sees only the first
  8 KB of a body, so `SizeRestrictions_BODY` only counts: bodies over 8 KB are blocked
  except on `waf_large_body_paths` (KYC upload, Proof of Reserve check), Content-Length
  over 64 KiB is blocked except on the KYC path, which is capped at 14,000,000 bytes.
  Optional source allowlists for the provider webhooks. Logs redact `authorization`,
  `cookie` and the webhook signature headers; WAF never logs bodies.

## Containers

Read-only root filesystem, uid/gid 65532, all capabilities dropped, init process, no
public IPs, no task role (the app calls no AWS API, so it gets no AWS credentials), one
execution role per task definition. Fargate rejects `dockerSecurityOptions`, so
`no-new-privileges` cannot be set; with no capabilities, a non-root user and distroless
images there is nothing to escalate to. Next.js writes only `/app/.next/cache`, a task
ephemeral-storage volume. Fargate gives it the ownership of the image's
`VOLUME /app/.next/cache` (owned by 65532 in `frontend/Dockerfile`); without that
declaration it would be root:root 0755 and unwritable.

Egress: the backend may reach `backend_egress_cidrs` on 443 through the NAT gateways
(`egress_public_ips` are the source addresses providers allowlist). AWS APIs go through
VPC endpoints whose policies only admit principals of this account. If a provider
cannot publish stable ranges, do not fall back to `0.0.0.0/0`: put AWS Network
Firewall (domain allowlist with TLS SNI inspection) or an egress proxy in front of the
NAT gateways.

## State and account baseline

The state holds the generated `sc_app` password and `DATABASE_URL`: keep it in an S3
bucket encrypted with a CMK, versioned, private, TLS-only and readable only by the
deployment role (see `backend.hcl.example`); locking uses `use_lockfile`.

Not created here, expected from the account baseline (landing zone): organization
CloudTrail (all regions, log file validation, data events on the state bucket),
GuardDuty (with RDS Protection and ECS Runtime Monitoring), AWS Config and Security Hub
(FSBP, CIS), IAM Access Analyzer, account-level S3 Block Public Access, EBS default
encryption, SCPs restricting regions to the residency region and protecting the
security services, AWS Backup with vault lock (cross-account copies of RDS snapshots),
a log archive account, and alarms/notifications (ALB 5xx and target health, RDS
CPU/storage/connections, proxy borrow timeouts, WAF blocks, ECS task failures),
Session Manager preferences (idle timeout, optional KMS encryption), and an SCP that
denies shell sessions on instances tagged `Component = dba-access`.
Optional: Shield Advanced on the ALB.

## Validation

```bash
terraform fmt -check -recursive
terraform init -backend=false && terraform validate
tflint --config=../.github/linters/.tflint.hcl --init && tflint --config=../.github/linters/.tflint.hcl
checkov -d . --framework terraform
trivy config .
```

Every scanner suppression is inline, next to its resource, with the reason
(`#checkov:skip=` / `#trivy:ignore:`). `.terraform.lock.hcl` pins the providers with
`zh:` hashes for every platform; refresh it with
`terraform providers lock -platform=linux_amd64 -platform=linux_arm64 -platform=darwin_amd64 -platform=darwin_arm64 -platform=windows_amd64`.
