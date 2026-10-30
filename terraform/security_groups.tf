# Security group matrix (stateful; anything not listed is denied):
#
#   alb       in : 443, 80 from alb_ingress_cidrs (80 only redirects to 443)
#             out: backend 8080, frontend 3000
#   backend   in : 8080 from alb
#             out: proxy 5432; VPC endpoints 443; S3 (ECR layers) 443; backend_egress_cidrs 443
#   frontend  in : 3000 from alb
#             out: VPC endpoints 443; S3 (ECR layers) 443
#   migrate   in : none
#             out: db 5432; VPC endpoints 443; S3 (ECR layers) 443
#   db_proxy  in : 5432 from backend
#             out: db 5432
#   db        in : 5432 from db_proxy and migrate only
#             out: none
#   endpoints in : 443 from backend, frontend, migrate
#             out: none
#
# A security group created by Terraform starts without the default allow-all egress rule.

resource "aws_security_group" "alb" {
  name        = "${local.name}-alb"
  description = "Internet-facing ALB: HTTPS (and HTTP redirect) in, app containers out"
  vpc_id      = aws_vpc.this.id

  tags = { Name = "${local.name}-alb" }
}

resource "aws_security_group" "backend" {
  name        = "${local.name}-backend"
  description = "Backend tasks: 8080 from the ALB; out to RDS Proxy, AWS endpoints and providers"
  vpc_id      = aws_vpc.this.id

  tags = { Name = "${local.name}-backend" }
}

resource "aws_security_group" "frontend" {
  name        = "${local.name}-frontend"
  description = "Frontend tasks: 3000 from the ALB; out to AWS endpoints only"
  vpc_id      = aws_vpc.this.id

  tags = { Name = "${local.name}-frontend" }
}

resource "aws_security_group" "migrate" {
  #checkov:skip=CKV2_AWS_5:Attached at run time: the one-off migrate task is launched with aws ecs run-task using this group (see the migrate_run_task_command output).
  name        = "${local.name}-migrate"
  description = "One-off migrate task: no inbound; out to the RDS instance and AWS endpoints"
  vpc_id      = aws_vpc.this.id

  tags = { Name = "${local.name}-migrate" }
}

resource "aws_security_group" "db_proxy" {
  #checkov:skip=CKV2_AWS_5:Attached to aws_db_proxy.this (vpc_security_group_ids); checkov does not model RDS Proxy as an attachment.
  name        = "${local.name}-db-proxy"
  description = "RDS Proxy: 5432 from the backend only; out to the RDS instance"
  vpc_id      = aws_vpc.this.id

  tags = { Name = "${local.name}-db-proxy" }
}

resource "aws_security_group" "db" {
  name        = "${local.name}-db"
  description = "RDS PostgreSQL: 5432 from RDS Proxy and the migrate task only; no egress"
  vpc_id      = aws_vpc.this.id

  tags = { Name = "${local.name}-db" }
}

resource "aws_security_group" "endpoints" {
  name        = "${local.name}-vpc-endpoints"
  description = "Interface VPC endpoints: 443 from the ECS tasks"
  vpc_id      = aws_vpc.this.id

  tags = { Name = "${local.name}-vpc-endpoints" }
}

# -----------------------------------------------------------------------------------------
# alb
# -----------------------------------------------------------------------------------------

resource "aws_vpc_security_group_ingress_rule" "alb_https" {
  for_each = toset(var.alb_ingress_cidrs)

  security_group_id = aws_security_group.alb.id
  description       = "HTTPS from clients"
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
  cidr_ipv4         = each.value

  tags = { Name = "${local.name}-alb-https-${each.value}" }
}

resource "aws_vpc_security_group_ingress_rule" "alb_http" {
  #checkov:skip=CKV_AWS_260:Port 80 only answers a 301 redirect to HTTPS (aws_lb_listener.http has no forward action); it is kept so first visits by host name reach the HTTPS site, after which HSTS applies.
  for_each = toset(var.alb_ingress_cidrs)

  security_group_id = aws_security_group.alb.id
  description       = "HTTP from clients (redirected to HTTPS)"
  ip_protocol       = "tcp"
  from_port         = 80
  to_port           = 80
  cidr_ipv4         = each.value

  tags = { Name = "${local.name}-alb-http-${each.value}" }
}

resource "aws_vpc_security_group_egress_rule" "alb_to_backend" {
  security_group_id            = aws_security_group.alb.id
  description                  = "To backend tasks"
  ip_protocol                  = "tcp"
  from_port                    = 8080
  to_port                      = 8080
  referenced_security_group_id = aws_security_group.backend.id

  tags = { Name = "${local.name}-alb-to-backend" }
}

resource "aws_vpc_security_group_egress_rule" "alb_to_frontend" {
  security_group_id            = aws_security_group.alb.id
  description                  = "To frontend tasks"
  ip_protocol                  = "tcp"
  from_port                    = 3000
  to_port                      = 3000
  referenced_security_group_id = aws_security_group.frontend.id

  tags = { Name = "${local.name}-alb-to-frontend" }
}

# -----------------------------------------------------------------------------------------
# backend
# -----------------------------------------------------------------------------------------

resource "aws_vpc_security_group_ingress_rule" "backend_from_alb" {
  security_group_id            = aws_security_group.backend.id
  description                  = "HTTP from the ALB"
  ip_protocol                  = "tcp"
  from_port                    = 8080
  to_port                      = 8080
  referenced_security_group_id = aws_security_group.alb.id

  tags = { Name = "${local.name}-backend-from-alb" }
}

resource "aws_vpc_security_group_egress_rule" "backend_to_proxy" {
  security_group_id            = aws_security_group.backend.id
  description                  = "PostgreSQL through RDS Proxy"
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
  referenced_security_group_id = aws_security_group.db_proxy.id

  tags = { Name = "${local.name}-backend-to-proxy" }
}

resource "aws_vpc_security_group_egress_rule" "backend_providers" {
  for_each = toset(var.backend_egress_cidrs)

  security_group_id = aws_security_group.backend.id
  description       = "HTTPS to SPEI / KYC provider APIs (via NAT)"
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
  cidr_ipv4         = each.value

  tags = { Name = "${local.name}-backend-providers-${each.value}" }
}

# -----------------------------------------------------------------------------------------
# frontend
# -----------------------------------------------------------------------------------------

resource "aws_vpc_security_group_ingress_rule" "frontend_from_alb" {
  security_group_id            = aws_security_group.frontend.id
  description                  = "HTTP from the ALB"
  ip_protocol                  = "tcp"
  from_port                    = 3000
  to_port                      = 3000
  referenced_security_group_id = aws_security_group.alb.id

  tags = { Name = "${local.name}-frontend-from-alb" }
}

# -----------------------------------------------------------------------------------------
# migrate
# -----------------------------------------------------------------------------------------

resource "aws_vpc_security_group_egress_rule" "migrate_to_db" {
  security_group_id            = aws_security_group.migrate.id
  description                  = "PostgreSQL to the RDS instance (schema owner, direct)"
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
  referenced_security_group_id = aws_security_group.db.id

  tags = { Name = "${local.name}-migrate-to-db" }
}

# -----------------------------------------------------------------------------------------
# AWS APIs for all ECS tasks: interface endpoints and the S3 gateway endpoint (ECR layers)
# -----------------------------------------------------------------------------------------

locals {
  task_security_groups = {
    backend  = aws_security_group.backend.id
    frontend = aws_security_group.frontend.id
    migrate  = aws_security_group.migrate.id
  }
}

resource "aws_vpc_security_group_egress_rule" "tasks_to_endpoints" {
  for_each = local.task_security_groups

  security_group_id            = each.value
  description                  = "HTTPS to interface VPC endpoints (ECR, Secrets Manager, Logs, ...)"
  ip_protocol                  = "tcp"
  from_port                    = 443
  to_port                      = 443
  referenced_security_group_id = aws_security_group.endpoints.id

  tags = { Name = "${local.name}-${each.key}-to-endpoints" }
}

resource "aws_vpc_security_group_egress_rule" "tasks_to_s3" {
  for_each = local.task_security_groups

  security_group_id = each.value
  description       = "HTTPS to S3 through the gateway endpoint (ECR image layers)"
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
  prefix_list_id    = aws_vpc_endpoint.s3.prefix_list_id

  tags = { Name = "${local.name}-${each.key}-to-s3" }
}

resource "aws_vpc_security_group_ingress_rule" "endpoints_from_tasks" {
  for_each = local.task_security_groups

  security_group_id            = aws_security_group.endpoints.id
  description                  = "HTTPS from ${each.key} tasks"
  ip_protocol                  = "tcp"
  from_port                    = 443
  to_port                      = 443
  referenced_security_group_id = each.value

  tags = { Name = "${local.name}-endpoints-from-${each.key}" }
}

# -----------------------------------------------------------------------------------------
# db_proxy and db
# -----------------------------------------------------------------------------------------

resource "aws_vpc_security_group_ingress_rule" "proxy_from_backend" {
  security_group_id            = aws_security_group.db_proxy.id
  description                  = "PostgreSQL from backend tasks"
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
  referenced_security_group_id = aws_security_group.backend.id

  tags = { Name = "${local.name}-proxy-from-backend" }
}

resource "aws_vpc_security_group_egress_rule" "proxy_to_db" {
  security_group_id            = aws_security_group.db_proxy.id
  description                  = "PostgreSQL to the RDS instance"
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
  referenced_security_group_id = aws_security_group.db.id

  tags = { Name = "${local.name}-proxy-to-db" }
}

resource "aws_vpc_security_group_ingress_rule" "db_from_proxy" {
  security_group_id            = aws_security_group.db.id
  description                  = "PostgreSQL from RDS Proxy (application login)"
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
  referenced_security_group_id = aws_security_group.db_proxy.id

  tags = { Name = "${local.name}-db-from-proxy" }
}

resource "aws_vpc_security_group_ingress_rule" "db_from_migrate" {
  security_group_id            = aws_security_group.db.id
  description                  = "PostgreSQL from the migrate task (schema owner)"
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
  referenced_security_group_id = aws_security_group.migrate.id

  tags = { Name = "${local.name}-db-from-migrate" }
}
