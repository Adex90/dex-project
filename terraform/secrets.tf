# Secrets Manager, all encrypted with the secrets CMK. Nothing secret is written in code
# or tfvars.
#
# 1. Generated here (VALUES ARE IN THE TERRAFORM STATE, which is why the state lives in
#    a CMK-encrypted, versioned S3 bucket that only the deployment role can read):
#      db-app/credentials   {"username": "sc_app", "password": "..."}: RDS Proxy
#                           authentication and DB_APP_PASSWORD for the migrate task
#      backend/DATABASE_URL postgres://sc_app:...@<proxy endpoint>:5432/<db>?sslmode=verify-full
# 2. Created EMPTY here, populated out of band (never in state; see README.md):
#      backend/JWT_SECRET, backend/SPEI_WEBHOOK_SECRETO, backend/KYC_WEBHOOK_SECRETO,
#      optional backend/*_ANTERIOR (rotation) and migrate/BOOTSTRAP_OPERADOR_PASSWORD.
#    ECS cannot start a task whose secret has no value: populate them before the
#    services scale above zero.
# 3. Managed by RDS: the master (schema owner) password, generated and rotated by RDS
#    (manage_master_user_password); Terraform never sees it.

resource "random_password" "db_app" {
  # Letters and digits only: the value is embedded in a URL and passed through env vars.
  # 48 characters of [A-Za-z0-9] carry about 285 bits of entropy.
  length      = 48
  special     = false
  min_lower   = 4
  min_upper   = 4
  min_numeric = 4
}

resource "aws_secretsmanager_secret" "db_app" {
  #checkov:skip=CKV2_AWS_57:Rotating this password is a coordinated procedure (new value here, migrate task sets the SCRAM verifier, backend redeploy); a rotation Lambda would bypass the migrate task that owns the login. See README.md.
  name                    = "${local.name}/db-app/credentials"
  description             = "PostgreSQL login ${var.db_app_username}: RDS Proxy authentication and DB_APP_PASSWORD for the migrate task"
  kms_key_id              = aws_kms_key.secrets.arn
  recovery_window_in_days = var.secret_recovery_window_days

  tags = { Name = "${local.name}/db-app/credentials", Component = "database" }
}

resource "aws_secretsmanager_secret_version" "db_app" {
  secret_id = aws_secretsmanager_secret.db_app.id
  secret_string = jsonencode({
    username = var.db_app_username
    password = random_password.db_app.result
  })
}

resource "aws_secretsmanager_secret" "backend_database_url" {
  #checkov:skip=CKV2_AWS_57:Derived from db-app/credentials and rotated with it (see README.md).
  name                    = "${local.name}/backend/DATABASE_URL"
  description             = "DATABASE_URL of the backend service: ${var.db_app_username} through RDS Proxy, TLS verify-full"
  kms_key_id              = aws_kms_key.secrets.arn
  recovery_window_in_days = var.secret_recovery_window_days

  tags = { Name = "${local.name}/backend/DATABASE_URL", Component = "backend" }
}

resource "aws_secretsmanager_secret_version" "backend_database_url" {
  secret_id = aws_secretsmanager_secret.backend_database_url.id
  # The RDS Proxy certificate is issued by ACM under Amazon Root CA 1, which is in the
  # webpki root store sqlx uses (tls-rustls), so verify-full needs no extra CA here.
  secret_string = "postgres://${var.db_app_username}:${urlencode(random_password.db_app.result)}@${aws_db_proxy.this.endpoint}:5432/${var.db_name}?sslmode=verify-full"
}

# Populated out of band. Values must pass the backend's checks (>= 32 characters, random):
#   openssl rand -hex 64 | tr -d '\n' | aws secretsmanager put-secret-value \
#     --secret-id <arn> --secret-string file:///dev/stdin
locals {
  backend_app_secrets = merge(
    {
      JWT_SECRET           = "JWT_SECRET: HS256 signing key of the backend"
      SPEI_WEBHOOK_SECRETO = "SPEI_WEBHOOK_SECRETO: HMAC-SHA256 key shared with the SPEI provider"
      KYC_WEBHOOK_SECRETO  = "KYC_WEBHOOK_SECRETO: HMAC-SHA256 key shared with the biometric provider"
    },
    var.enable_previous_webhook_secrets ? {
      SPEI_WEBHOOK_SECRETO_ANTERIOR = "SPEI_WEBHOOK_SECRETO_ANTERIOR: previous SPEI webhook key, accepted during a rotation"
      KYC_WEBHOOK_SECRETO_ANTERIOR  = "KYC_WEBHOOK_SECRETO_ANTERIOR: previous KYC webhook key, accepted during a rotation"
    } : {},
  )
}

resource "aws_secretsmanager_secret" "backend" {
  #checkov:skip=CKV2_AWS_57:JWT and webhook HMAC keys are rotated by a coordinated procedure (the *_ANTERIOR overlap keeps provider webhooks valid); automatic rotation would break the providers' copies. See README.md.
  for_each = local.backend_app_secrets

  name                    = "${local.name}/backend/${each.key}"
  description             = each.value
  kms_key_id              = aws_kms_key.secrets.arn
  recovery_window_in_days = var.secret_recovery_window_days

  tags = { Name = "${local.name}/backend/${each.key}", Component = "backend" }
}

resource "aws_secretsmanager_secret" "bootstrap_operator_password" {
  #checkov:skip=CKV2_AWS_57:One-time bootstrap credential; the operator changes the password after the first login.
  count = var.bootstrap_operator_email == null ? 0 : 1

  name                    = "${local.name}/migrate/BOOTSTRAP_OPERADOR_PASSWORD"
  description             = "BOOTSTRAP_OPERADOR_PASSWORD: password of the first operator, created by the migrate task"
  kms_key_id              = aws_kms_key.secrets.arn
  recovery_window_in_days = var.secret_recovery_window_days

  tags = { Name = "${local.name}/migrate/BOOTSTRAP_OPERADOR_PASSWORD", Component = "migrate" }
}
