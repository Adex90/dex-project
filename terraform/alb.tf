# Internet-facing ALB, same routing contract as the nginx edge:
#   /api/* and /salud -> backend (8080); everything else -> frontend (3000)
# so the frontend is built with NEXT_PUBLIC_API_URL=https://<domain_name> (no path).
#
# TLS 1.3 only by default, HTTP -> HTTPS redirect, invalid header fields dropped,
# strictest desync mitigation, WAF in front (waf.tf), access and connection logs to S3.

resource "aws_lb" "this" {
  name               = "${local.name}-alb"
  load_balancer_type = "application"
  internal           = false
  ip_address_type    = "ipv4"
  security_groups    = [aws_security_group.alb.id]
  subnets            = [for s in aws_subnet.public : s.id]

  drop_invalid_header_fields = true
  desync_mitigation_mode     = "strictest"
  enable_deletion_protection = var.alb_deletion_protection
  enable_http2               = true
  enable_waf_fail_open       = false # if WAF cannot evaluate a request, it is not forwarded
  idle_timeout               = var.alb_idle_timeout_seconds

  # The backend (HTTP_PROXY_CONFIABLE=true) takes the client IP from the last
  # X-Forwarded-For hop, which the ALB appends; without a port suffix.
  xff_header_processing_mode = "append"
  enable_xff_client_port     = false
  preserve_host_header       = false

  access_logs {
    bucket  = aws_s3_bucket.alb_logs.id
    prefix  = "access"
    enabled = true
  }

  connection_logs {
    bucket  = aws_s3_bucket.alb_logs.id
    prefix  = "connection"
    enabled = true
  }

  tags = { Name = "${local.name}-alb", Component = "edge" }

  depends_on = [aws_s3_bucket_policy.alb_logs]
}

# -----------------------------------------------------------------------------------------
# Target groups (IP targets: Fargate awsvpc ENIs). TLS terminates at the ALB; target
# traffic stays inside the VPC, between the ALB and app security groups only.
# -----------------------------------------------------------------------------------------

resource "aws_lb_target_group" "backend" {
  #checkov:skip=CKV_AWS_378:TLS terminates at the ALB. The backend serves plain HTTP/1.1 on 8080 in private subnets, reachable only from the ALB security group.
  name                          = "${local.name}-be"
  target_type                   = "ip"
  protocol                      = "HTTP"
  protocol_version              = "HTTP1"
  port                          = 8080
  vpc_id                        = aws_vpc.this.id
  deregistration_delay          = 30
  load_balancing_algorithm_type = "least_outstanding_requests"

  # Readiness: /salud checks the database (the container health check, `salud vivo`,
  # does not, so a database outage does not make ECS kill every task).
  health_check {
    enabled             = true
    protocol            = "HTTP"
    port                = "traffic-port"
    path                = "/salud"
    matcher             = "200"
    interval            = 15
    timeout             = 5
    healthy_threshold   = 2
    unhealthy_threshold = 3
  }

  tags = { Name = "${local.name}-be", Component = "backend" }
}

resource "aws_lb_target_group" "frontend" {
  #checkov:skip=CKV_AWS_378:TLS terminates at the ALB. The frontend serves plain HTTP on 3000 in private subnets, reachable only from the ALB security group.
  name                 = "${local.name}-fe"
  target_type          = "ip"
  protocol             = "HTTP"
  protocol_version     = "HTTP1"
  port                 = 3000
  vpc_id               = aws_vpc.this.id
  deregistration_delay = 30

  health_check {
    enabled             = true
    protocol            = "HTTP"
    port                = "traffic-port"
    path                = "/"
    matcher             = "200"
    interval            = 15
    timeout             = 5
    healthy_threshold   = 2
    unhealthy_threshold = 3
  }

  tags = { Name = "${local.name}-fe", Component = "frontend" }
}

# -----------------------------------------------------------------------------------------
# Listeners
# -----------------------------------------------------------------------------------------

resource "aws_lb_listener" "http" {
  load_balancer_arn = aws_lb.this.arn
  port              = 80
  protocol          = "HTTP"

  default_action {
    type = "redirect"

    redirect {
      protocol    = "HTTPS"
      port        = "443"
      status_code = "HTTP_301"
    }
  }

  tags = { Name = "${local.name}-http-redirect", Component = "edge" }
}

resource "aws_lb_listener" "https" {
  load_balancer_arn = aws_lb.this.arn
  port              = 443
  protocol          = "HTTPS"
  ssl_policy        = var.alb_ssl_policy
  certificate_arn   = local.certificate_arn

  # Security headers on every response (the nginx edge sets the same ones).
  routing_http_response_server_enabled                         = false
  routing_http_response_strict_transport_security_header_value = var.strict_transport_security
  routing_http_response_content_security_policy_header_value   = var.content_security_policy
  routing_http_response_x_content_type_options_header_value    = "nosniff"
  routing_http_response_x_frame_options_header_value           = "DENY"

  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.frontend.arn
  }

  tags = { Name = "${local.name}-https", Component = "edge" }
}

resource "aws_lb_listener_rule" "backend" {
  listener_arn = aws_lb_listener.https.arn
  priority     = 10

  action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.backend.arn
  }

  condition {
    path_pattern {
      values = ["/api/*", "/salud"]
    }
  }

  tags = { Name = "${local.name}-api-to-backend", Component = "edge" }
}

# -----------------------------------------------------------------------------------------
# Certificate and DNS
#   acm_certificate_arn set:  that certificate is used as is.
#   otherwise:                a DNS-validated ACM certificate is requested; with
#                             route53_zone_id the validation records are created here,
#                             without it they must be created at the DNS provider (see the
#                             acm_validation_records output) while apply waits.
# -----------------------------------------------------------------------------------------

resource "aws_acm_certificate" "this" {
  count = var.acm_certificate_arn == null ? 1 : 0

  domain_name               = var.domain_name
  subject_alternative_names = var.subject_alternative_names
  validation_method         = "DNS"
  key_algorithm             = "RSA_2048" # widest client support (provider webhook clients)

  options {
    certificate_transparency_logging_preference = "ENABLED"
  }

  tags = { Name = var.domain_name, Component = "edge" }

  lifecycle {
    create_before_destroy = true
  }
}

locals {
  acm_validation_records = {
    for dvo in flatten([for c in aws_acm_certificate.this : tolist(c.domain_validation_options)]) :
    dvo.domain_name => {
      name   = dvo.resource_record_name
      type   = dvo.resource_record_type
      record = dvo.resource_record_value
    }
  }
}

resource "aws_route53_record" "acm_validation" {
  for_each = var.route53_zone_id == null ? {} : local.acm_validation_records

  zone_id         = var.route53_zone_id
  name            = each.value.name
  type            = each.value.type
  records         = [each.value.record]
  ttl             = 300
  allow_overwrite = true # a name and its wildcard share one validation record
}

resource "aws_acm_certificate_validation" "this" {
  count = var.acm_certificate_arn == null ? 1 : 0

  certificate_arn         = aws_acm_certificate.this[0].arn
  validation_record_fqdns = var.route53_zone_id == null ? null : [for r in aws_route53_record.acm_validation : r.fqdn]

  timeouts {
    create = "2h"
  }
}

locals {
  certificate_arn = var.acm_certificate_arn != null ? var.acm_certificate_arn : one(aws_acm_certificate_validation.this[*].certificate_arn)
}

resource "aws_route53_record" "app" {
  count = var.route53_zone_id == null ? 0 : 1

  zone_id = var.route53_zone_id
  name    = var.domain_name
  type    = "A"

  alias {
    name                   = aws_lb.this.dns_name
    zone_id                = aws_lb.this.zone_id
    evaluate_target_health = true
  }
}
