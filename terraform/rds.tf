# RDS for PostgreSQL 16 in the isolated data subnets, reachable only from RDS Proxy (the
# backend's path) and from the one-off migrate task (the schema owner's path).
#
# Two identities, as in the README's production model:
#   master user (var.db_master_username) = schema owner. Password generated, stored and
#     rotated by RDS in Secrets Manager. Only the migrate task can read it; it reaches
#     the instance endpoint directly (DATABASE_URL_PROPIETARIO).
#   application login (var.db_app_username, NOINHERIT, member of the sc_* roles) =
#     created/updated by the migrate task from DB_APP_PASSWORD; the service connects as
#     it through RDS Proxy (DATABASE_URL).

locals {
  db_identifier = "${local.name}-postgres"
  db_proxy_name = "${local.name}-proxy"

  # sqlx (tls-rustls) trusts only the webpki roots. RDS instance certificates come from
  # the private RDS CA (rds-ca-rsa2048-g1), so the migrate task's direct connection needs
  # the regional RDS bundle to use sslmode=verify-full. sqlx reads PGSSLROOTCERT and
  # accepts inline PEM (sqlx-core CertificateInput), adding it to the webpki roots.
  rds_ca_bundle_pem = var.rds_ca_bundle_pem != null ? var.rds_ca_bundle_pem : one(data.http.rds_ca_bundle[*].response_body)
}

data "http" "rds_ca_bundle" {
  count = var.rds_ca_bundle_pem == null ? 1 : 0

  url                = "https://truststore.pki.rds.amazonaws.com/${var.region}/${var.region}-bundle.pem"
  request_timeout_ms = 30000

  retry {
    attempts = 3
  }

  lifecycle {
    postcondition {
      condition     = self.status_code == 200 && strcontains(self.response_body, "-----BEGIN CERTIFICATE-----")
      error_message = "Could not download the RDS CA bundle for ${var.region}; set rds_ca_bundle_pem instead."
    }
  }
}

resource "aws_db_subnet_group" "this" {
  name        = "${local.name}-data"
  description = "Isolated data subnets (no route to the internet)"
  subnet_ids  = [for s in aws_subnet.data : s.id]

  tags = { Name = "${local.name}-data" }
}

# -----------------------------------------------------------------------------------------
# Parameter group
#
# Deliberately NOT set server-wide: statement_timeout and lock_timeout. Migrations (the
# 0009 AML migration alone is ~300 KB of DDL) must not be cut by a global limit; the
# backend sets per-session limits itself (statement_timeout 30s, lock_timeout 5s,
# idle_in_transaction_session_timeout 60s) when it opens a connection.
# -----------------------------------------------------------------------------------------

resource "aws_db_parameter_group" "postgres16" {
  name        = "${local.name}-postgres16"
  family      = "postgres16"
  description = "${local.name}: TLS required, SCRAM only, audit logging"

  # --- Transport and authentication ---------------------------------------------------
  parameter {
    name         = "rds.force_ssl" # reject non-TLS connections
    value        = "1"
    apply_method = "pending-reboot"
  }

  parameter {
    name         = "rds.accepted_password_auth_method" # SCRAM only, never md5
    value        = "scram"
    apply_method = "pending-reboot"
  }

  parameter {
    name         = "password_encryption"
    value        = "scram-sha-256"
    apply_method = "immediate"
  }

  parameter {
    name         = "ssl_min_protocol_version"
    value        = "TLSv1.2"
    apply_method = "immediate"
  }

  # --- Logging: connections, DDL, slow statements, lock waits, temp files, autovacuum --
  parameter {
    name         = "log_connections"
    value        = "1"
    apply_method = "immediate"
  }

  parameter {
    name         = "log_disconnections"
    value        = "1"
    apply_method = "immediate"
  }

  parameter {
    name         = "log_statement"
    value        = "ddl"
    apply_method = "immediate"
  }

  parameter {
    name         = "log_min_duration_statement"
    value        = tostring(var.db_log_min_duration_ms)
    apply_method = "immediate"
  }

  parameter {
    name         = "log_parameter_max_length" # never log bind values (customer data)
    value        = "0"
    apply_method = "immediate"
  }

  parameter {
    name         = "log_lock_waits"
    value        = "1"
    apply_method = "immediate"
  }

  parameter {
    name         = "log_temp_files" # KB
    value        = "10240"
    apply_method = "immediate"
  }

  parameter {
    name         = "log_autovacuum_min_duration" # ms
    value        = "10000"
    apply_method = "immediate"
  }

  parameter {
    name         = "track_io_timing"
    value        = "1"
    apply_method = "immediate"
  }

  # --- Backstop for sessions that leave a transaction open (the backend sets 60s) ------
  parameter {
    name         = "idle_in_transaction_session_timeout" # ms
    value        = "300000"
    apply_method = "immediate"
  }

  tags = { Name = "${local.name}-postgres16" }

  lifecycle {
    create_before_destroy = true
  }
}

# -----------------------------------------------------------------------------------------
# Enhanced Monitoring role
# -----------------------------------------------------------------------------------------

data "aws_iam_policy_document" "rds_monitoring_assume" {
  statement {
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["monitoring.rds.amazonaws.com"]
    }

    condition {
      test     = "StringEquals"
      variable = "aws:SourceAccount"
      values   = [local.account_id]
    }
  }
}

resource "aws_iam_role" "rds_monitoring" {
  name               = "${local.name}-rds-monitoring"
  assume_role_policy = data.aws_iam_policy_document.rds_monitoring_assume.json

  tags = { Name = "${local.name}-rds-monitoring" }
}

resource "aws_iam_role_policy_attachment" "rds_monitoring" {
  role       = aws_iam_role.rds_monitoring.name
  policy_arn = "arn:${local.partition}:iam::aws:policy/service-role/AmazonRDSEnhancedMonitoringRole"
}

# -----------------------------------------------------------------------------------------
# Instance
# -----------------------------------------------------------------------------------------

#trivy:ignore:AWS-0176 IAM database authentication is not used: SCRAM through RDS Proxy for the service, the RDS-managed secret for the owner.
resource "aws_db_instance" "this" {
  #checkov:skip=CKV_AWS_161:IAM database authentication is not used: the service authenticates with SCRAM through RDS Proxy and the owner with the RDS-managed secret. Leaving it off avoids an unused login path.
  identifier     = local.db_identifier
  engine         = "postgres"
  engine_version = var.db_engine_version
  instance_class = var.db_instance_class

  db_name                       = var.db_name
  username                      = var.db_master_username
  manage_master_user_password   = true
  master_user_secret_kms_key_id = aws_kms_key.secrets.arn
  port                          = 5432

  allocated_storage     = var.db_allocated_storage
  max_allocated_storage = var.db_max_allocated_storage
  storage_type          = "gp3"
  storage_encrypted     = true
  kms_key_id            = aws_kms_key.rds.arn

  multi_az               = var.db_multi_az
  db_subnet_group_name   = aws_db_subnet_group.this.name
  vpc_security_group_ids = [aws_security_group.db.id]
  publicly_accessible    = false
  network_type           = "IPV4"
  ca_cert_identifier     = "rds-ca-rsa2048-g1"
  parameter_group_name   = aws_db_parameter_group.postgres16.name

  # 35 days of automated backups = point-in-time recovery to any second in that window.
  backup_retention_period   = var.db_backup_retention_days
  backup_window             = var.db_backup_window
  maintenance_window        = var.db_maintenance_window
  copy_tags_to_snapshot     = true
  delete_automated_backups  = false
  deletion_protection       = var.db_deletion_protection
  skip_final_snapshot       = false
  final_snapshot_identifier = "${local.db_identifier}-final"

  performance_insights_enabled          = true
  performance_insights_kms_key_id       = aws_kms_key.rds.arn
  performance_insights_retention_period = var.db_performance_insights_retention_days
  monitoring_interval                   = var.db_monitoring_interval
  monitoring_role_arn                   = aws_iam_role.rds_monitoring.arn
  enabled_cloudwatch_logs_exports       = ["postgresql", "upgrade"]

  iam_database_authentication_enabled = false
  auto_minor_version_upgrade          = true
  allow_major_version_upgrade         = false
  apply_immediately                   = var.db_apply_immediately

  tags = { Name = local.db_identifier, Component = "database" }

  depends_on = [
    aws_cloudwatch_log_group.rds,
    aws_iam_role_policy_attachment.rds_monitoring,
  ]
}

# -----------------------------------------------------------------------------------------
# RDS Proxy for the application login (TLS required)
#
# PINNING CAVEAT (read before tuning): the backend's sqlx pool runs session-level SET
# statements when it opens a connection (application_name, lock_timeout,
# statement_timeout, idle_in_transaction_session_timeout, time zone), uses named
# prepared statements, and opens every transaction with SET LOCAL ROLE. RDS Proxy pins a
# client connection to its database connection for any of these, so every backend
# connection is pinned from the moment it opens: there is no transaction-level
# multiplexing. What the proxy still provides:
#   - failover handling: the endpoint stays stable during a Multi-AZ failover, and the
#     proxy routes new connections to the new primary without waiting for DNS;
#   - connection capping (max_connections_percent): an autoscaling storm cannot exhaust
#     the database's max_connections; excess clients wait up to connection_borrow_timeout;
#   - TLS with an ACM (public CA) certificate, so verify-full works with webpki roots;
#   - credentials held in Secrets Manager.
# Size it accordingly: backend_max_tasks x backend_db_max_connections must stay below
# max_connections x db_proxy_max_connections_percent / 100.
# -----------------------------------------------------------------------------------------

data "aws_iam_policy_document" "db_proxy_assume" {
  statement {
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["rds.amazonaws.com"]
    }

    condition {
      test     = "StringEquals"
      variable = "aws:SourceAccount"
      values   = [local.account_id]
    }
  }
}

resource "aws_iam_role" "db_proxy" {
  name               = "${local.name}-rds-proxy"
  assume_role_policy = data.aws_iam_policy_document.db_proxy_assume.json

  tags = { Name = "${local.name}-rds-proxy" }
}

data "aws_iam_policy_document" "db_proxy" {
  statement {
    sid       = "ReadApplicationLoginSecret"
    actions   = ["secretsmanager:GetSecretValue", "secretsmanager:DescribeSecret"]
    resources = [aws_secretsmanager_secret.db_app.arn]
  }

  statement {
    sid       = "DecryptThroughSecretsManager"
    actions   = ["kms:Decrypt"]
    resources = [aws_kms_key.secrets.arn]

    condition {
      test     = "StringEquals"
      variable = "kms:ViaService"
      values   = ["secretsmanager.${var.region}.amazonaws.com"]
    }
  }
}

resource "aws_iam_role_policy" "db_proxy" {
  name   = "read-db-app-secret"
  role   = aws_iam_role.db_proxy.id
  policy = data.aws_iam_policy_document.db_proxy.json
}

resource "aws_db_proxy" "this" {
  name                   = local.db_proxy_name
  engine_family          = "POSTGRESQL"
  require_tls            = true
  debug_logging          = false # debug logs would contain SQL text
  idle_client_timeout    = var.db_proxy_idle_client_timeout
  role_arn               = aws_iam_role.db_proxy.arn
  vpc_subnet_ids         = [for s in aws_subnet.data : s.id]
  vpc_security_group_ids = [aws_security_group.db_proxy.id]

  auth {
    description               = "Application login ${var.db_app_username}"
    auth_scheme               = "SECRETS"
    secret_arn                = aws_secretsmanager_secret.db_app.arn
    iam_auth                  = "DISABLED"
    client_password_auth_type = "POSTGRES_SCRAM_SHA_256"
  }

  tags = { Name = local.db_proxy_name, Component = "database" }

  depends_on = [
    aws_cloudwatch_log_group.rds_proxy,
    aws_iam_role_policy.db_proxy,
  ]
}

resource "aws_db_proxy_default_target_group" "this" {
  db_proxy_name = aws_db_proxy.this.name

  connection_pool_config {
    max_connections_percent      = var.db_proxy_max_connections_percent
    max_idle_connections_percent = min(50, var.db_proxy_max_connections_percent)
    connection_borrow_timeout    = 120
  }
}

resource "aws_db_proxy_target" "this" {
  db_proxy_name          = aws_db_proxy.this.name
  target_group_name      = aws_db_proxy_default_target_group.this.name
  db_instance_identifier = aws_db_instance.this.identifier
}
