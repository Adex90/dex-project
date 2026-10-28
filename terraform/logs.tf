# CloudWatch log groups, all KMS-encrypted with the same retention. RDS and RDS Proxy log
# groups are created here first, so AWS does not create them unencrypted and unbounded.

resource "aws_cloudwatch_log_group" "ecs" {
  for_each = toset(["backend", "frontend", "migrate"])

  name                        = "/ecs/${local.name}/${each.key}"
  retention_in_days           = var.log_retention_days
  kms_key_id                  = aws_kms_key.logs.arn
  deletion_protection_enabled = var.log_group_deletion_protection

  tags = { Name = "/ecs/${local.name}/${each.key}", Component = each.key }
}

resource "aws_cloudwatch_log_group" "vpc_flow" {
  name                        = "/vpc/${local.name}/flow-logs"
  retention_in_days           = var.log_retention_days
  kms_key_id                  = aws_kms_key.logs.arn
  deletion_protection_enabled = var.log_group_deletion_protection

  tags = { Name = "/vpc/${local.name}/flow-logs", Component = "network" }
}

resource "aws_cloudwatch_log_group" "rds" {
  for_each = toset(["postgresql", "upgrade"])

  name                        = "/aws/rds/instance/${local.db_identifier}/${each.key}"
  retention_in_days           = var.log_retention_days
  kms_key_id                  = aws_kms_key.logs.arn
  deletion_protection_enabled = var.log_group_deletion_protection

  tags = { Name = "/aws/rds/instance/${local.db_identifier}/${each.key}", Component = "database" }
}

resource "aws_cloudwatch_log_group" "rds_proxy" {
  name                        = "/aws/rds/proxy/${local.db_proxy_name}"
  retention_in_days           = var.log_retention_days
  kms_key_id                  = aws_kms_key.logs.arn
  deletion_protection_enabled = var.log_group_deletion_protection

  tags = { Name = "/aws/rds/proxy/${local.db_proxy_name}", Component = "database" }
}

# WAF requires the aws-waf-logs- prefix.
resource "aws_cloudwatch_log_group" "waf" {
  name                        = "aws-waf-logs-${local.name}"
  retention_in_days           = var.log_retention_days
  kms_key_id                  = aws_kms_key.logs.arn
  deletion_protection_enabled = var.log_group_deletion_protection

  tags = { Name = "aws-waf-logs-${local.name}", Component = "edge" }
}

# -----------------------------------------------------------------------------------------
# S3 bucket for ALB access and connection logs
# -----------------------------------------------------------------------------------------

locals {
  alb_logs_bucket = "${local.name}-alb-logs-${local.account_id}-${var.region}"

  # Regions launched before August 2022 deliver ALB logs from a regional ELB account;
  # newer regions (mx-central-1 among them) use the log delivery service principal.
  elb_log_delivery_accounts = {
    "us-east-1"      = "127311923021"
    "us-east-2"      = "033677994240"
    "us-west-1"      = "027434742980"
    "us-west-2"      = "797873946194"
    "af-south-1"     = "098369216593"
    "ap-east-1"      = "754344448648"
    "ap-southeast-3" = "589379963580"
    "ap-south-1"     = "718504428378"
    "ap-northeast-3" = "383597477331"
    "ap-northeast-2" = "600734575887"
    "ap-southeast-1" = "114774131450"
    "ap-southeast-2" = "783225319266"
    "ap-northeast-1" = "582318560864"
    "ca-central-1"   = "985666609251"
    "eu-central-1"   = "054676820928"
    "eu-west-1"      = "156460612806"
    "eu-west-2"      = "652711504416"
    "eu-south-1"     = "635631232127"
    "eu-west-3"      = "009996457667"
    "eu-north-1"     = "897822967062"
    "me-south-1"     = "076674570225"
    "sa-east-1"      = "507241528517"
  }
  elb_log_delivery_account = lookup(local.elb_log_delivery_accounts, var.region, null)

  alb_log_object_arns = [
    "${aws_s3_bucket.alb_logs.arn}/access/AWSLogs/${local.account_id}/*",
    "${aws_s3_bucket.alb_logs.arn}/connection/AWSLogs/${local.account_id}/*",
  ]
}

#trivy:ignore:AWS-0089 Access-log destination; logging it into a further bucket adds no audit value (CloudTrail data events cover object access).
#trivy:ignore:AWS-0132 Elastic Load Balancing delivers access logs only to SSE-S3 (AES-256) buckets; SSE-KMS is rejected.
resource "aws_s3_bucket" "alb_logs" {
  #checkov:skip=CKV_AWS_18:This bucket is the access-log destination; logging it into itself or a further bucket adds no audit value. CloudTrail data events (account baseline) cover object access.
  #checkov:skip=CKV_AWS_144:Cross-region replication would copy client IPs and request paths out of the data-residency region. Versioning and the lifecycle rule protect the logs in-region.
  #checkov:skip=CKV_AWS_145:Elastic Load Balancing can only deliver access logs to buckets encrypted with SSE-S3 (AES-256); SSE-KMS is rejected.
  #checkov:skip=CKV2_AWS_62:No consumer for object events; logs are queried in place (Athena) when needed.
  bucket        = local.alb_logs_bucket
  force_destroy = false

  tags = { Name = local.alb_logs_bucket, Component = "edge" }

  lifecycle {
    precondition {
      condition     = length(local.alb_logs_bucket) <= 63
      error_message = "The ALB log bucket name ${local.alb_logs_bucket} exceeds 63 characters; shorten project or environment."
    }
  }
}

resource "aws_s3_bucket_ownership_controls" "alb_logs" {
  bucket = aws_s3_bucket.alb_logs.id

  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

resource "aws_s3_bucket_public_access_block" "alb_logs" {
  bucket = aws_s3_bucket.alb_logs.id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_versioning" "alb_logs" {
  bucket = aws_s3_bucket.alb_logs.id

  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "alb_logs" {
  bucket = aws_s3_bucket.alb_logs.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256" # the only encryption ALB log delivery supports
    }
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "alb_logs" {
  bucket = aws_s3_bucket.alb_logs.id

  rule {
    id     = "expire-load-balancer-logs"
    status = "Enabled"

    filter {}

    expiration {
      days = var.alb_log_retention_days
    }

    noncurrent_version_expiration {
      noncurrent_days = 30
    }

    abort_incomplete_multipart_upload {
      days_after_initiation = 7
    }
  }

  depends_on = [aws_s3_bucket_versioning.alb_logs]
}

data "aws_iam_policy_document" "alb_logs" {
  statement {
    sid       = "AllowLoadBalancerLogDelivery"
    actions   = ["s3:PutObject"]
    resources = local.alb_log_object_arns

    principals {
      type        = "Service"
      identifiers = ["logdelivery.elasticloadbalancing.amazonaws.com"]
    }
  }

  dynamic "statement" {
    for_each = local.elb_log_delivery_account == null ? [] : [local.elb_log_delivery_account]
    content {
      sid       = "AllowRegionalElbAccountLogDelivery"
      actions   = ["s3:PutObject"]
      resources = local.alb_log_object_arns

      principals {
        type        = "AWS"
        identifiers = ["arn:${local.partition}:iam::${statement.value}:root"]
      }
    }
  }

  statement {
    sid       = "DenyInsecureTransport"
    effect    = "Deny"
    actions   = ["s3:*"]
    resources = [aws_s3_bucket.alb_logs.arn, "${aws_s3_bucket.alb_logs.arn}/*"]

    principals {
      type        = "*"
      identifiers = ["*"]
    }

    condition {
      test     = "Bool"
      variable = "aws:SecureTransport"
      values   = ["false"]
    }
  }
}

resource "aws_s3_bucket_policy" "alb_logs" {
  bucket = aws_s3_bucket.alb_logs.id
  policy = data.aws_iam_policy_document.alb_logs.json

  depends_on = [aws_s3_bucket_public_access_block.alb_logs]
}
