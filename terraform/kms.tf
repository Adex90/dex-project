# Customer managed keys, one per purpose, with automatic yearly rotation.
#
#   rds     RDS storage and Performance Insights
#   secrets Secrets Manager: application secrets and the RDS-managed master user secret
#   logs    CloudWatch Logs (ECS, VPC flow, RDS, RDS Proxy, WAF)
#   ecr     ECR image encryption
#   ebs     EBS volumes (the DBA bastion's root volume)
#
# Key policies delegate to IAM in this account (the root statement); IAM policies in this
# configuration then grant each role only the key it needs. RDS, ECR, Secrets Manager and
# EC2 (EBS) use the keys through grants created on behalf of the deploying principal. Only
# CloudWatch Logs needs an explicit service statement.

locals {
  kms_root_statement = {
    Sid       = "EnableIamPoliciesOfThisAccount"
    Effect    = "Allow"
    Principal = { AWS = "arn:${local.partition}:iam::${local.account_id}:root" }
    Action    = "kms:*"
    Resource  = "*"
  }
}

resource "aws_kms_key" "rds" {
  description             = "${local.name}: RDS PostgreSQL storage and Performance Insights"
  enable_key_rotation     = true
  rotation_period_in_days = 365
  deletion_window_in_days = var.kms_deletion_window_days

  policy = jsonencode({
    Version   = "2012-10-17"
    Statement = [local.kms_root_statement]
  })

  tags = { Name = "${local.name}-rds" }
}

resource "aws_kms_alias" "rds" {
  name          = "alias/${local.name}/rds"
  target_key_id = aws_kms_key.rds.key_id
}

resource "aws_kms_key" "secrets" {
  description             = "${local.name}: Secrets Manager (application secrets, RDS master user secret)"
  enable_key_rotation     = true
  rotation_period_in_days = 365
  deletion_window_in_days = var.kms_deletion_window_days

  policy = jsonencode({
    Version   = "2012-10-17"
    Statement = [local.kms_root_statement]
  })

  tags = { Name = "${local.name}-secrets" }
}

resource "aws_kms_alias" "secrets" {
  name          = "alias/${local.name}/secrets"
  target_key_id = aws_kms_key.secrets.key_id
}

resource "aws_kms_key" "logs" {
  description             = "${local.name}: CloudWatch Logs"
  enable_key_rotation     = true
  rotation_period_in_days = 365
  deletion_window_in_days = var.kms_deletion_window_days

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      local.kms_root_statement,
      {
        Sid       = "AllowCloudWatchLogsForThisAccountsLogGroups"
        Effect    = "Allow"
        Principal = { Service = "logs.${var.region}.amazonaws.com" }
        Action = [
          "kms:Encrypt",
          "kms:Decrypt",
          "kms:ReEncrypt*",
          "kms:GenerateDataKey*",
          "kms:Describe*",
        ]
        Resource = "*"
        Condition = {
          ArnLike = {
            "kms:EncryptionContext:aws:logs:arn" = "arn:${local.partition}:logs:${var.region}:${local.account_id}:log-group:*"
          }
        }
      },
    ]
  })

  tags = { Name = "${local.name}-logs" }
}

resource "aws_kms_alias" "logs" {
  name          = "alias/${local.name}/logs"
  target_key_id = aws_kms_key.logs.key_id
}

resource "aws_kms_key" "ecr" {
  description             = "${local.name}: ECR images"
  enable_key_rotation     = true
  rotation_period_in_days = 365
  deletion_window_in_days = var.kms_deletion_window_days

  policy = jsonencode({
    Version   = "2012-10-17"
    Statement = [local.kms_root_statement]
  })

  tags = { Name = "${local.name}-ecr" }
}

resource "aws_kms_alias" "ecr" {
  name          = "alias/${local.name}/ecr"
  target_key_id = aws_kms_key.ecr.key_id
}

# Kept when dba_bastion_enabled is false: switching the bastion off and on must not
# schedule a key deletion every time.
resource "aws_kms_key" "ebs" {
  description             = "${local.name}: EBS volumes (DBA bastion)"
  enable_key_rotation     = true
  rotation_period_in_days = 365
  deletion_window_in_days = var.kms_deletion_window_days

  policy = jsonencode({
    Version   = "2012-10-17"
    Statement = [local.kms_root_statement]
  })

  tags = { Name = "${local.name}-ebs" }
}

resource "aws_kms_alias" "ebs" {
  name          = "alias/${local.name}/ebs"
  target_key_id = aws_kms_key.ebs.key_id
}
