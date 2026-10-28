# WAFv2 web ACL on the ALB.
#
# Rule order (lower priority number first):
#    0-1  provider webhook IP allowlists (only when the CIDR lists are set)
#   10    AWS IP reputation list
#   11    AWS anonymous IP list (Tor/anonymising proxies blocked, hosting providers counted)
#   20-23 rate limits per client IP: auth, KYC upload, webhooks, rest of the API
#   30    AWS common rule set, with SizeRestrictions_BODY switched to COUNT
#   31-33 body size, scoped (see below)
#   40-41 AWS known bad inputs, AWS SQL injection
#
# Body size. Behind an ALB, WAF inspects only the first 8 KB of a body, and the managed
# SizeRestrictions_BODY rule blocks anything larger, which would block the KYC upload
# (POST /api/v1/kyc/verificar, up to 12 MB of JSON) and large Proof of Reserve checks
# (/api/v1/reserva/verificar, about 10 KB). So the managed rule only labels (COUNT) and:
#   31  blocks bodies over 8 KB (its label) except on var.waf_large_body_paths;
#   32  blocks a Content-Length over 64 KiB (the backend's limit) except on the KYC path;
#   33  blocks a Content-Length of 14,000,000 bytes or more on the KYC path (12 MiB limit
#       plus base64/JSON slack; the backend still enforces the exact limits).
#
# Path matches use the raw URI path without transformations on purpose: the backend
# (axum) routes on the raw path, so an encoded or re-cased variant reaches no handler.

locals {
  waf_spei_webhook_prefix = "/api/v1/spei/webhook/"
  waf_kyc_webhook_path    = "/api/v1/kyc/webhook"
  waf_kyc_verify_path     = "/api/v1/kyc/verificar"

  waf_large_body_regex = "^(${join("|", var.waf_large_body_paths)})$"

  # Decimal Content-Length values greater than 65536.
  waf_over_64k_regex = "^0*(6553[7-9]|655[4-9][0-9]|65[6-9][0-9]{2}|6[6-9][0-9]{3}|[7-9][0-9]{4}|[1-9][0-9]{5,})$"
  # Decimal Content-Length values of 14,000,000 or more.
  waf_over_14m_regex = "^0*([1-9][0-9]{8,}|1[4-9][0-9]{6}|[2-9][0-9]{7})$"

  # Same envelope as the backend's errors: {"error": {"codigo": ..., "mensaje": ...}}.
  waf_responses = {
    prohibido = {
      status = 403
      body   = jsonencode({ error = { codigo = "PROHIBIDO", mensaje = "solicitud rechazada" } })
    }
    demasiadas_peticiones = {
      status = 429
      body   = jsonencode({ error = { codigo = "DEMASIADAS_PETICIONES", mensaje = "demasiadas solicitudes; intenta más tarde" } })
    }
    cuerpo_demasiado_grande = {
      status = 413
      body   = jsonencode({ error = { codigo = "CUERPO_DEMASIADO_GRANDE", mensaje = "el cuerpo de la petición excede el máximo permitido" } })
    }
  }

  waf_rate_rules = {
    auth = {
      priority = 20
      limit    = var.waf_rate_limit_auth
    }
    kyc-verify = {
      priority = 21
      limit    = var.waf_rate_limit_kyc_verify
    }
    webhooks = {
      priority = 22
      limit    = var.waf_rate_limit_webhooks
    }
    api = {
      priority = 23
      limit    = var.waf_rate_limit_api
    }
  }
}

resource "aws_wafv2_ip_set" "spei_webhook" {
  count = length(var.spei_webhook_allowed_cidrs) > 0 ? 1 : 0

  name               = "${local.name}-spei-webhook"
  description        = "SPEI provider source addresses"
  scope              = "REGIONAL"
  ip_address_version = "IPV4"
  addresses          = var.spei_webhook_allowed_cidrs

  tags = { Name = "${local.name}-spei-webhook", Component = "edge" }
}

resource "aws_wafv2_ip_set" "kyc_webhook" {
  count = length(var.kyc_webhook_allowed_cidrs) > 0 ? 1 : 0

  name               = "${local.name}-kyc-webhook"
  description        = "Biometric (KYC) provider source addresses"
  scope              = "REGIONAL"
  ip_address_version = "IPV4"
  addresses          = var.kyc_webhook_allowed_cidrs

  tags = { Name = "${local.name}-kyc-webhook", Component = "edge" }
}

resource "aws_wafv2_web_acl" "this" {
  name        = "${local.name}-alb"
  description = "Synthetic Capital ALB: managed rules, rate limits, scoped body size, webhook allowlists"
  scope       = "REGIONAL"

  default_action {
    allow {}
  }

  dynamic "custom_response_body" {
    for_each = local.waf_responses
    content {
      key          = custom_response_body.key
      content      = custom_response_body.value.body
      content_type = "APPLICATION_JSON"
    }
  }

  # --- 0-1: provider webhook allowlists ---------------------------------------------------

  dynamic "rule" {
    for_each = aws_wafv2_ip_set.spei_webhook
    content {
      name     = "spei-webhook-allowlist"
      priority = 0

      action {
        block {
          custom_response {
            response_code            = local.waf_responses.prohibido.status
            custom_response_body_key = "prohibido"
          }
        }
      }

      statement {
        and_statement {
          statement {
            byte_match_statement {
              positional_constraint = "STARTS_WITH"
              search_string         = local.waf_spei_webhook_prefix
              field_to_match {
                uri_path {}
              }
              text_transformation {
                priority = 0
                type     = "NONE"
              }
            }
          }
          statement {
            not_statement {
              statement {
                ip_set_reference_statement {
                  arn = rule.value.arn
                }
              }
            }
          }
        }
      }

      visibility_config {
        cloudwatch_metrics_enabled = true
        metric_name                = "${local.name}-spei-webhook-allowlist"
        sampled_requests_enabled   = true
      }
    }
  }

  dynamic "rule" {
    for_each = aws_wafv2_ip_set.kyc_webhook
    content {
      name     = "kyc-webhook-allowlist"
      priority = 1

      action {
        block {
          custom_response {
            response_code            = local.waf_responses.prohibido.status
            custom_response_body_key = "prohibido"
          }
        }
      }

      statement {
        and_statement {
          statement {
            byte_match_statement {
              positional_constraint = "EXACTLY"
              search_string         = local.waf_kyc_webhook_path
              field_to_match {
                uri_path {}
              }
              text_transformation {
                priority = 0
                type     = "NONE"
              }
            }
          }
          statement {
            not_statement {
              statement {
                ip_set_reference_statement {
                  arn = rule.value.arn
                }
              }
            }
          }
        }
      }

      visibility_config {
        cloudwatch_metrics_enabled = true
        metric_name                = "${local.name}-kyc-webhook-allowlist"
        sampled_requests_enabled   = true
      }
    }
  }

  # --- 10: IP reputation --------------------------------------------------------------------

  rule {
    name     = "aws-ip-reputation"
    priority = 10

    override_action {
      none {}
    }

    statement {
      managed_rule_group_statement {
        vendor_name = "AWS"
        name        = "AWSManagedRulesAmazonIpReputationList"
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "${local.name}-aws-ip-reputation"
      sampled_requests_enabled   = true
    }
  }

  # --- 11: anonymising networks ---------------------------------------------------------
  # Tor exit nodes and anonymising proxies are blocked (var.waf_block_anonymous_ips);
  # data-centre addresses are only counted, because the SPEI and KYC providers call the
  # webhooks from hosting networks.

  rule {
    name     = "aws-anonymous-ip"
    priority = 11

    override_action {
      none {}
    }

    statement {
      managed_rule_group_statement {
        vendor_name = "AWS"
        name        = "AWSManagedRulesAnonymousIpList"

        rule_action_override {
          name = "HostingProviderIPList"
          action_to_use {
            count {}
          }
        }

        dynamic "rule_action_override" {
          for_each = var.waf_block_anonymous_ips ? [] : [1]
          content {
            name = "AnonymousIPList"
            action_to_use {
              count {}
            }
          }
        }
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "${local.name}-aws-anonymous-ip"
      sampled_requests_enabled   = true
    }
  }

  # --- 20-23: rate limits per client IP ------------------------------------------------------
  # The ALB is the first hop (no CDN), so the source IP is the client's own address.

  rule {
    name     = "rate-auth"
    priority = local.waf_rate_rules.auth.priority

    action {
      block {
        custom_response {
          response_code            = local.waf_responses.demasiadas_peticiones.status
          custom_response_body_key = "demasiadas_peticiones"
        }
      }
    }

    statement {
      rate_based_statement {
        limit                 = local.waf_rate_rules.auth.limit
        evaluation_window_sec = var.waf_rate_window_seconds
        aggregate_key_type    = "IP"

        scope_down_statement {
          byte_match_statement {
            positional_constraint = "STARTS_WITH"
            search_string         = "/api/v1/auth/"
            field_to_match {
              uri_path {}
            }
            text_transformation {
              priority = 0
              type     = "NONE"
            }
          }
        }
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "${local.name}-rate-auth"
      sampled_requests_enabled   = true
    }
  }

  rule {
    name     = "rate-kyc-verify"
    priority = local.waf_rate_rules["kyc-verify"].priority

    action {
      block {
        custom_response {
          response_code            = local.waf_responses.demasiadas_peticiones.status
          custom_response_body_key = "demasiadas_peticiones"
        }
      }
    }

    statement {
      rate_based_statement {
        limit                 = local.waf_rate_rules["kyc-verify"].limit
        evaluation_window_sec = var.waf_rate_window_seconds
        aggregate_key_type    = "IP"

        scope_down_statement {
          byte_match_statement {
            positional_constraint = "EXACTLY"
            search_string         = local.waf_kyc_verify_path
            field_to_match {
              uri_path {}
            }
            text_transformation {
              priority = 0
              type     = "NONE"
            }
          }
        }
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "${local.name}-rate-kyc-verify"
      sampled_requests_enabled   = true
    }
  }

  rule {
    name     = "rate-webhooks"
    priority = local.waf_rate_rules.webhooks.priority

    action {
      block {
        custom_response {
          response_code            = local.waf_responses.demasiadas_peticiones.status
          custom_response_body_key = "demasiadas_peticiones"
        }
      }
    }

    statement {
      rate_based_statement {
        limit                 = local.waf_rate_rules.webhooks.limit
        evaluation_window_sec = var.waf_rate_window_seconds
        aggregate_key_type    = "IP"

        scope_down_statement {
          or_statement {
            statement {
              byte_match_statement {
                positional_constraint = "STARTS_WITH"
                search_string         = local.waf_spei_webhook_prefix
                field_to_match {
                  uri_path {}
                }
                text_transformation {
                  priority = 0
                  type     = "NONE"
                }
              }
            }
            statement {
              byte_match_statement {
                positional_constraint = "EXACTLY"
                search_string         = local.waf_kyc_webhook_path
                field_to_match {
                  uri_path {}
                }
                text_transformation {
                  priority = 0
                  type     = "NONE"
                }
              }
            }
          }
        }
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "${local.name}-rate-webhooks"
      sampled_requests_enabled   = true
    }
  }

  rule {
    name     = "rate-api"
    priority = local.waf_rate_rules.api.priority

    action {
      block {
        custom_response {
          response_code            = local.waf_responses.demasiadas_peticiones.status
          custom_response_body_key = "demasiadas_peticiones"
        }
      }
    }

    statement {
      rate_based_statement {
        limit                 = local.waf_rate_rules.api.limit
        evaluation_window_sec = var.waf_rate_window_seconds
        aggregate_key_type    = "IP"

        # /api/ minus the provider webhooks (they have their own limit above).
        scope_down_statement {
          and_statement {
            statement {
              byte_match_statement {
                positional_constraint = "STARTS_WITH"
                search_string         = "/api/"
                field_to_match {
                  uri_path {}
                }
                text_transformation {
                  priority = 0
                  type     = "NONE"
                }
              }
            }
            statement {
              not_statement {
                statement {
                  byte_match_statement {
                    positional_constraint = "STARTS_WITH"
                    search_string         = local.waf_spei_webhook_prefix
                    field_to_match {
                      uri_path {}
                    }
                    text_transformation {
                      priority = 0
                      type     = "NONE"
                    }
                  }
                }
              }
            }
            statement {
              not_statement {
                statement {
                  byte_match_statement {
                    positional_constraint = "EXACTLY"
                    search_string         = local.waf_kyc_webhook_path
                    field_to_match {
                      uri_path {}
                    }
                    text_transformation {
                      priority = 0
                      type     = "NONE"
                    }
                  }
                }
              }
            }
          }
        }
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "${local.name}-rate-api"
      sampled_requests_enabled   = true
    }
  }

  # --- 30: AWS common rule set (SizeRestrictions_BODY only labels) ---------------------------

  rule {
    name     = "aws-common"
    priority = 30

    override_action {
      none {}
    }

    statement {
      managed_rule_group_statement {
        vendor_name = "AWS"
        name        = "AWSManagedRulesCommonRuleSet"

        rule_action_override {
          name = "SizeRestrictions_BODY"
          action_to_use {
            count {}
          }
        }
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "${local.name}-aws-common"
      sampled_requests_enabled   = true
    }
  }

  # --- 31-33: scoped body size ---------------------------------------------------------------

  rule {
    name     = "body-over-8k-outside-large-body-paths"
    priority = 31

    action {
      block {
        custom_response {
          response_code            = local.waf_responses.cuerpo_demasiado_grande.status
          custom_response_body_key = "cuerpo_demasiado_grande"
        }
      }
    }

    statement {
      and_statement {
        statement {
          label_match_statement {
            scope = "LABEL"
            key   = "awswaf:managed:aws:core-rule-set:SizeRestrictions_Body"
          }
        }
        statement {
          not_statement {
            statement {
              regex_match_statement {
                regex_string = local.waf_large_body_regex
                field_to_match {
                  uri_path {}
                }
                text_transformation {
                  priority = 0
                  type     = "NONE"
                }
              }
            }
          }
        }
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "${local.name}-body-over-8k"
      sampled_requests_enabled   = true
    }
  }

  rule {
    name     = "content-length-over-64k"
    priority = 32

    action {
      block {
        custom_response {
          response_code            = local.waf_responses.cuerpo_demasiado_grande.status
          custom_response_body_key = "cuerpo_demasiado_grande"
        }
      }
    }

    statement {
      and_statement {
        statement {
          regex_match_statement {
            regex_string = local.waf_over_64k_regex
            field_to_match {
              single_header {
                name = "content-length"
              }
            }
            text_transformation {
              priority = 0
              type     = "NONE"
            }
          }
        }
        statement {
          not_statement {
            statement {
              byte_match_statement {
                positional_constraint = "EXACTLY"
                search_string         = local.waf_kyc_verify_path
                field_to_match {
                  uri_path {}
                }
                text_transformation {
                  priority = 0
                  type     = "NONE"
                }
              }
            }
          }
        }
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "${local.name}-content-length-over-64k"
      sampled_requests_enabled   = true
    }
  }

  rule {
    name     = "kyc-content-length-over-14m"
    priority = 33

    action {
      block {
        custom_response {
          response_code            = local.waf_responses.cuerpo_demasiado_grande.status
          custom_response_body_key = "cuerpo_demasiado_grande"
        }
      }
    }

    statement {
      and_statement {
        statement {
          byte_match_statement {
            positional_constraint = "EXACTLY"
            search_string         = local.waf_kyc_verify_path
            field_to_match {
              uri_path {}
            }
            text_transformation {
              priority = 0
              type     = "NONE"
            }
          }
        }
        statement {
          regex_match_statement {
            regex_string = local.waf_over_14m_regex
            field_to_match {
              single_header {
                name = "content-length"
              }
            }
            text_transformation {
              priority = 0
              type     = "NONE"
            }
          }
        }
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "${local.name}-kyc-content-length-over-14m"
      sampled_requests_enabled   = true
    }
  }

  # --- 40-41: more managed rule groups ------------------------------------------------------

  rule {
    name     = "aws-known-bad-inputs"
    priority = 40

    override_action {
      none {}
    }

    statement {
      managed_rule_group_statement {
        vendor_name = "AWS"
        name        = "AWSManagedRulesKnownBadInputsRuleSet"
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "${local.name}-aws-known-bad-inputs"
      sampled_requests_enabled   = true
    }
  }

  rule {
    name     = "aws-sqli"
    priority = 41

    override_action {
      none {}
    }

    statement {
      managed_rule_group_statement {
        vendor_name = "AWS"
        name        = "AWSManagedRulesSQLiRuleSet"
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "${local.name}-aws-sqli"
      sampled_requests_enabled   = true
    }
  }

  visibility_config {
    cloudwatch_metrics_enabled = true
    metric_name                = "${local.name}-alb"
    sampled_requests_enabled   = true
  }

  tags = { Name = "${local.name}-alb", Component = "edge" }
}

resource "aws_wafv2_web_acl_association" "alb" {
  resource_arn = aws_lb.this.arn
  web_acl_arn  = aws_wafv2_web_acl.this.arn
}

# -----------------------------------------------------------------------------------------
# Logging (KMS-encrypted log group). Credentials and signatures are redacted; WAF never
# logs request bodies, so KYC images and personal data stay out of these logs.
# -----------------------------------------------------------------------------------------

resource "aws_cloudwatch_log_resource_policy" "waf" {
  policy_name = "${local.name}-waf-log-delivery"
  policy_document = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Sid       = "AllowWafLogDelivery"
      Effect    = "Allow"
      Principal = { Service = "delivery.logs.amazonaws.com" }
      Action    = ["logs:CreateLogStream", "logs:PutLogEvents"]
      Resource  = "${aws_cloudwatch_log_group.waf.arn}:*"
      Condition = {
        ArnLike      = { "aws:SourceArn" = "arn:${local.partition}:logs:${var.region}:${local.account_id}:*" }
        StringEquals = { "aws:SourceAccount" = local.account_id }
      }
    }]
  })
}

resource "aws_wafv2_web_acl_logging_configuration" "this" {
  resource_arn            = aws_wafv2_web_acl.this.arn
  log_destination_configs = [aws_cloudwatch_log_group.waf.arn]

  redacted_fields {
    single_header {
      name = "authorization"
    }
  }

  redacted_fields {
    single_header {
      name = "cookie"
    }
  }

  redacted_fields {
    single_header {
      name = "x-kyc-firma"
    }
  }

  redacted_fields {
    single_header {
      name = "x-spei-firma"
    }
  }

  dynamic "logging_filter" {
    for_each = var.waf_log_all_requests ? [] : [1]
    content {
      default_behavior = "DROP"

      filter {
        behavior    = "KEEP"
        requirement = "MEETS_ANY"

        condition {
          action_condition {
            action = "BLOCK"
          }
        }

        condition {
          action_condition {
            action = "COUNT"
          }
        }
      }
    }
  }

  depends_on = [aws_cloudwatch_log_resource_policy.waf]
}
