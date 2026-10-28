# Synthetic Capital: AWS reference architecture.
#
#   internet -> WAF -> ALB (public subnets, TLS 1.3)
#                       |-- /api/*, /salud -> backend  (ECS Fargate, private app subnets, :8080)
#                       `-- everything else -> frontend (ECS Fargate, private app subnets, :3000)
#   backend -> RDS Proxy (TLS) -> RDS PostgreSQL 16 Multi-AZ   (private data subnets, no route out)
#   migrate (one-off task)     -> RDS instance directly, as the schema owner
#
# File map: vpc.tf (network, endpoints, flow logs, NACLs), security_groups.tf, kms.tf,
# logs.tf (log groups, ALB log bucket), secrets.tf, rds.tf (database + proxy), ecr.tf,
# ecs.tf (cluster, task definitions, services, autoscaling), alb.tf (load balancer, TLS,
# DNS), waf.tf, outputs.tf.

data "aws_caller_identity" "current" {}

data "aws_partition" "current" {}

data "aws_availability_zones" "available" {
  state = "available"

  # Local Zones and Wavelength Zones are opt-in; the bank runs only in regular zones.
  filter {
    name   = "opt-in-status"
    values = ["opt-in-not-required"]
  }
}

locals {
  name       = "${var.project}-${var.environment}"
  account_id = data.aws_caller_identity.current.account_id
  partition  = data.aws_partition.current.partition

  azs = slice(
    data.aws_availability_zones.available.names,
    0,
    min(var.availability_zone_count, length(data.aws_availability_zones.available.names)),
  )

  tags = merge(var.tags, {
    Project     = var.project
    Environment = var.environment
    ManagedBy   = "terraform"
  })

  # ECR image reference: an immutable tag, or a digest (sha256:...).
  backend_image  = "${aws_ecr_repository.this["backend"].repository_url}${startswith(var.backend_image_tag, "sha256:") ? "@" : ":"}${var.backend_image_tag}"
  frontend_image = "${aws_ecr_repository.this["frontend"].repository_url}${startswith(var.frontend_image_tag, "sha256:") ? "@" : ":"}${var.frontend_image_tag}"

  public_url = "https://${var.domain_name}"
}
