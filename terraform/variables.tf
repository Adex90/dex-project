# -----------------------------------------------------------------------------------------
# Account, region and naming
# -----------------------------------------------------------------------------------------

variable "region" {
  description = "AWS region for every resource. No default on purpose: data residency is a regulatory decision (mx-central-1 keeps customer data in Mexico; any other region must be approved by compliance)."
  type        = string

  validation {
    condition     = can(regex("^[a-z]{2}(-gov)?-[a-z]+-[0-9]$", var.region))
    error_message = "region must be an AWS region code such as mx-central-1."
  }
}

variable "aws_account_id" {
  description = "The only AWS account this configuration may touch (provider allowed_account_ids)."
  type        = string

  validation {
    condition     = can(regex("^[0-9]{12}$", var.aws_account_id))
    error_message = "aws_account_id must be a 12-digit AWS account ID."
  }
}

variable "project" {
  description = "Project name, used as the prefix of every resource name."
  type        = string
  default     = "synthetic-capital"

  validation {
    # Load balancer and target group names are limited to 32 characters.
    condition     = can(regex("^[a-z][a-z0-9-]{1,16}[a-z0-9]$", var.project))
    error_message = "project must be 3-18 characters: lowercase letters, digits and hyphens, starting with a letter."
  }
}

variable "environment" {
  description = "Environment name (prod, staging, ...), the second part of every resource name."
  type        = string
  default     = "prod"

  validation {
    condition     = can(regex("^[a-z][a-z0-9]{1,7}$", var.environment))
    error_message = "environment must be 2-8 lowercase letters or digits, starting with a letter."
  }
}

variable "tags" {
  description = "Extra tags for every resource (for example Owner, CostCenter, DataClassification)."
  type        = map(string)
  default     = {}
}

# -----------------------------------------------------------------------------------------
# Network
# -----------------------------------------------------------------------------------------

variable "availability_zone_count" {
  description = "Number of availability zones (2 or 3). Every tier gets one subnet per zone."
  type        = number
  default     = 3

  validation {
    condition     = contains([2, 3], var.availability_zone_count)
    error_message = "availability_zone_count must be 2 or 3."
  }
}

variable "vpc_cidr" {
  description = "VPC CIDR block, /16 to /18. Split per zone into /19 app, /22 public and /22 data subnets (for a /16)."
  type        = string
  default     = "10.40.0.0/16"

  validation {
    condition     = can(cidrhost(var.vpc_cidr, 0)) && contains(["16", "17", "18"], try(split("/", var.vpc_cidr)[1], ""))
    error_message = "vpc_cidr must be a valid IPv4 CIDR block between /16 and /18."
  }
}

variable "single_nat_gateway" {
  description = "Use one NAT gateway instead of one per zone. Cheaper, but outbound traffic (provider calls) stops if that zone fails. Keep false in production."
  type        = bool
  default     = false
}

# -----------------------------------------------------------------------------------------
# Edge: DNS, TLS, load balancer
# -----------------------------------------------------------------------------------------

variable "domain_name" {
  description = "Public host name of the bank (for example bank.example.mx). The frontend must be built with NEXT_PUBLIC_API_URL=https://<domain_name>; the backend gets FRONTEND_ORIGIN=https://<domain_name>."
  type        = string

  validation {
    condition     = can(regex("^([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\\.)+[a-z]{2,63}$", var.domain_name))
    error_message = "domain_name must be a lowercase DNS name such as bank.example.mx."
  }
}

variable "subject_alternative_names" {
  description = "Extra names for the ACM certificate that this configuration requests (ignored when acm_certificate_arn is set)."
  type        = list(string)
  default     = []

  validation {
    condition     = alltrue([for n in var.subject_alternative_names : can(regex("^(\\*\\.)?([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\\.)+[a-z]{2,63}$", n))])
    error_message = "subject_alternative_names must be lowercase DNS names (a leading *. is allowed)."
  }
}

variable "acm_certificate_arn" {
  description = "ARN of an existing ACM certificate (same region) for domain_name. Null: request one with DNS validation."
  type        = string
  default     = null

  validation {
    condition     = var.acm_certificate_arn == null || can(regex("^arn:aws[a-z-]*:acm:[a-z0-9-]+:[0-9]{12}:certificate/[0-9a-f-]{36}$", var.acm_certificate_arn))
    error_message = "acm_certificate_arn must be an ACM certificate ARN."
  }
}

variable "route53_zone_id" {
  description = "Public Route 53 hosted zone for domain_name. Set: ACM validation records and the alias record are created here. Null: create them at your DNS provider (see the acm_validation_records and alb_dns_name outputs)."
  type        = string
  default     = null

  validation {
    condition     = var.route53_zone_id == null || can(regex("^Z[A-Z0-9]{1,31}$", var.route53_zone_id))
    error_message = "route53_zone_id must be a Route 53 hosted zone ID (Z...)."
  }
}

variable "alb_ingress_cidrs" {
  description = "IPv4 CIDR blocks allowed to reach the load balancer on 80/443. The internet by default; narrow it for a private launch."
  type        = list(string)
  default     = ["0.0.0.0/0"]

  validation {
    condition     = length(var.alb_ingress_cidrs) > 0 && alltrue([for c in var.alb_ingress_cidrs : can(cidrnetmask(c))])
    error_message = "alb_ingress_cidrs must be a non-empty list of IPv4 CIDR blocks."
  }
}

variable "alb_ssl_policy" {
  description = "ALB TLS policy. The default allows TLS 1.3 only (like the nginx edge). If a provider's webhook client cannot do TLS 1.3, use ELBSecurityPolicy-TLS13-1-2-Res-2021-06; the -PQ- variants add post-quantum key exchange."
  type        = string
  default     = "ELBSecurityPolicy-TLS13-1-3-2021-06"

  validation {
    condition     = can(regex("^ELBSecurityPolicy-TLS13-", var.alb_ssl_policy))
    error_message = "alb_ssl_policy must be a TLS 1.3-capable ELBSecurityPolicy-TLS13-* policy."
  }
}

variable "alb_idle_timeout_seconds" {
  description = "ALB idle timeout. Must exceed backend_http_request_timeout_seconds (a silent request must not be cut by the ALB first). The backend's idle keep-alive limit (HTTP_CABECERAS_SEG) is set 15 s above it, so the ALB never reuses a connection the backend already closed (502)."
  type        = number
  default     = 90

  validation {
    condition     = var.alb_idle_timeout_seconds >= 30 && var.alb_idle_timeout_seconds <= 600 && var.alb_idle_timeout_seconds > var.backend_http_request_timeout_seconds
    error_message = "alb_idle_timeout_seconds must be 30-600 and greater than backend_http_request_timeout_seconds."
  }
}

variable "alb_deletion_protection" {
  description = "Protect the load balancer from deletion."
  type        = bool
  default     = true
}

variable "strict_transport_security" {
  description = "Strict-Transport-Security header the HTTPS listener adds to every response."
  type        = string
  default     = "max-age=63072000; includeSubDomains"
}

variable "content_security_policy" {
  description = "Content-Security-Policy header the HTTPS listener adds to every response. Same policy as the nginx edge: Next.js static pages need 'unsafe-inline' scripts (no nonces); camera frames are blob:/data: images."
  type        = string
  default     = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' blob:; font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'; upgrade-insecure-requests"
}

# -----------------------------------------------------------------------------------------
# WAF
# -----------------------------------------------------------------------------------------

variable "waf_rate_window_seconds" {
  description = "Evaluation window of the rate-based rules."
  type        = number
  default     = 300

  validation {
    condition     = contains([60, 120, 300, 600], var.waf_rate_window_seconds)
    error_message = "waf_rate_window_seconds must be 60, 120, 300 or 600."
  }
}

variable "waf_rate_limit_auth" {
  description = "Requests per client IP and window to /api/v1/auth/ (login, sign-up). The backend also limits login to 5 attempts per account per 15 minutes."
  type        = number
  default     = 100

  validation {
    condition     = var.waf_rate_limit_auth >= 10 && var.waf_rate_limit_auth <= 2000000000
    error_message = "waf_rate_limit_auth must be at least 10."
  }
}

variable "waf_rate_limit_kyc_verify" {
  description = "Requests per client IP and window to POST /api/v1/kyc/verificar (up to 12 MB each)."
  type        = number
  default     = 30

  validation {
    condition     = var.waf_rate_limit_kyc_verify >= 10 && var.waf_rate_limit_kyc_verify <= 2000000000
    error_message = "waf_rate_limit_kyc_verify must be at least 10."
  }
}

variable "waf_rate_limit_webhooks" {
  description = "Requests per source IP and window to the provider webhooks (/api/v1/spei/webhook/*, /api/v1/kyc/webhook). Providers send bursts from few IPs, so this is separate from the general API limit."
  type        = number
  default     = 6000

  validation {
    condition     = var.waf_rate_limit_webhooks >= 10 && var.waf_rate_limit_webhooks <= 2000000000
    error_message = "waf_rate_limit_webhooks must be at least 10."
  }
}

variable "waf_rate_limit_api" {
  description = "Requests per client IP and window to the rest of /api/ (webhooks excluded)."
  type        = number
  default     = 3000

  validation {
    condition     = var.waf_rate_limit_api >= 10 && var.waf_rate_limit_api <= 2000000000
    error_message = "waf_rate_limit_api must be at least 10."
  }
}

variable "waf_large_body_paths" {
  description = "Exact paths whose request bodies may exceed 8 KB (the most WAF inspects behind an ALB). Every other path keeps the managed SizeRestrictions_BODY behaviour (block above 8 KB). The backend itself caps bodies at 64 KB, and at 12 MB on /api/v1/kyc/verificar."
  type        = list(string)
  default     = ["/api/v1/kyc/verificar", "/api/v1/reserva/verificar"]

  validation {
    condition     = length(var.waf_large_body_paths) > 0 && alltrue([for p in var.waf_large_body_paths : can(regex("^/[A-Za-z0-9/_-]+$", p))])
    error_message = "waf_large_body_paths must be non-empty and contain only absolute paths made of letters, digits, /, _ and -."
  }
}

variable "spei_webhook_allowed_cidrs" {
  description = "Source IPv4 CIDR blocks of the SPEI provider. Non-empty: /api/v1/spei/webhook/* is blocked for every other source. Empty: no allowlist (HMAC signatures still apply)."
  type        = list(string)
  default     = []

  validation {
    condition     = alltrue([for c in var.spei_webhook_allowed_cidrs : can(cidrnetmask(c))])
    error_message = "spei_webhook_allowed_cidrs must contain IPv4 CIDR blocks."
  }
}

variable "kyc_webhook_allowed_cidrs" {
  description = "Source IPv4 CIDR blocks of the biometric (KYC) provider. Non-empty: /api/v1/kyc/webhook is blocked for every other source."
  type        = list(string)
  default     = []

  validation {
    condition     = alltrue([for c in var.kyc_webhook_allowed_cidrs : can(cidrnetmask(c))])
    error_message = "kyc_webhook_allowed_cidrs must contain IPv4 CIDR blocks."
  }
}

variable "waf_log_all_requests" {
  description = "Log every request evaluated by the WAF. False: only requests that a rule blocked or counted (the ALB access logs already record every request)."
  type        = bool
  default     = false
}

# -----------------------------------------------------------------------------------------
# Containers (ECS Fargate)
# -----------------------------------------------------------------------------------------

variable "backend_image_tag" {
  description = "Backend image in this stack's ECR repository: an immutable tag (for example the git SHA) or a digest (sha256:...). Never 'latest'."
  type        = string

  validation {
    condition     = can(regex("^(sha256:[a-f0-9]{64}|[A-Za-z0-9_][A-Za-z0-9_.-]{0,127})$", var.backend_image_tag)) && var.backend_image_tag != "latest"
    error_message = "backend_image_tag must be an image tag other than 'latest', or a sha256:<64 hex> digest."
  }
}

variable "frontend_image_tag" {
  description = "Frontend image tag or digest (see backend_image_tag). The image must be built with NEXT_PUBLIC_API_URL=https://<domain_name>."
  type        = string

  validation {
    condition     = can(regex("^(sha256:[a-f0-9]{64}|[A-Za-z0-9_][A-Za-z0-9_.-]{0,127})$", var.frontend_image_tag)) && var.frontend_image_tag != "latest"
    error_message = "frontend_image_tag must be an image tag other than 'latest', or a sha256:<64 hex> digest."
  }
}

variable "cpu_architecture" {
  description = "CPU architecture of the images and the Fargate tasks."
  type        = string
  default     = "X86_64"

  validation {
    condition     = contains(["X86_64", "ARM64"], var.cpu_architecture)
    error_message = "cpu_architecture must be X86_64 or ARM64."
  }
}

variable "backend_cpu" {
  description = "Backend task CPU units (Fargate: 256, 512, 1024, 2048, 4096, ...)."
  type        = number
  default     = 512

  validation {
    condition     = contains([256, 512, 1024, 2048, 4096, 8192, 16384], var.backend_cpu)
    error_message = "backend_cpu must be a Fargate CPU value: 256, 512, 1024, 2048, 4096, 8192 or 16384."
  }
}

variable "backend_memory" {
  description = "Backend task memory in MiB (must be valid for backend_cpu)."
  type        = number
  default     = 1024

  validation {
    condition     = var.backend_memory >= 512 && var.backend_memory <= 122880
    error_message = "backend_memory must be between 512 and 122880 MiB."
  }
}

variable "frontend_cpu" {
  description = "Frontend task CPU units."
  type        = number
  default     = 256

  validation {
    condition     = contains([256, 512, 1024, 2048, 4096, 8192, 16384], var.frontend_cpu)
    error_message = "frontend_cpu must be a Fargate CPU value: 256, 512, 1024, 2048, 4096, 8192 or 16384."
  }
}

variable "frontend_memory" {
  description = "Frontend task memory in MiB."
  type        = number
  default     = 512

  validation {
    condition     = var.frontend_memory >= 512 && var.frontend_memory <= 122880
    error_message = "frontend_memory must be between 512 and 122880 MiB."
  }
}

variable "migrate_cpu" {
  description = "Migrate task CPU units."
  type        = number
  default     = 512

  validation {
    condition     = contains([256, 512, 1024, 2048, 4096, 8192, 16384], var.migrate_cpu)
    error_message = "migrate_cpu must be a Fargate CPU value: 256, 512, 1024, 2048, 4096, 8192 or 16384."
  }
}

variable "migrate_memory" {
  description = "Migrate task memory in MiB."
  type        = number
  default     = 1024

  validation {
    condition     = var.migrate_memory >= 512 && var.migrate_memory <= 122880
    error_message = "migrate_memory must be between 512 and 122880 MiB."
  }
}

variable "backend_min_tasks" {
  description = "Minimum (and initial) backend tasks. 0 for the first apply, before images, secrets and migrations exist; at least 2 in production (one per zone)."
  type        = number
  default     = 2

  validation {
    condition     = var.backend_min_tasks >= 0 && var.backend_min_tasks <= 50
    error_message = "backend_min_tasks must be between 0 and 50."
  }
}

variable "backend_max_tasks" {
  description = "Maximum backend tasks. Each task opens up to backend_db_max_connections connections, all pinned in RDS Proxy: keep max_tasks x connections below the proxy's share of the database's max_connections."
  type        = number
  default     = 6

  validation {
    condition     = var.backend_max_tasks >= 1 && var.backend_max_tasks <= 50 && var.backend_max_tasks >= var.backend_min_tasks
    error_message = "backend_max_tasks must be 1-50 and at least backend_min_tasks."
  }
}

variable "frontend_min_tasks" {
  description = "Minimum (and initial) frontend tasks. 0 for the first apply; at least 2 in production."
  type        = number
  default     = 2

  validation {
    condition     = var.frontend_min_tasks >= 0 && var.frontend_min_tasks <= 50
    error_message = "frontend_min_tasks must be between 0 and 50."
  }
}

variable "frontend_max_tasks" {
  description = "Maximum frontend tasks."
  type        = number
  default     = 4

  validation {
    condition     = var.frontend_max_tasks >= 1 && var.frontend_max_tasks <= 50 && var.frontend_max_tasks >= var.frontend_min_tasks
    error_message = "frontend_max_tasks must be 1-50 and at least frontend_min_tasks."
  }
}

variable "autoscaling_cpu_target" {
  description = "Target average CPU utilisation (%) for both services."
  type        = number
  default     = 60

  validation {
    condition     = var.autoscaling_cpu_target >= 20 && var.autoscaling_cpu_target <= 90
    error_message = "autoscaling_cpu_target must be between 20 and 90."
  }
}

variable "backend_requests_per_target" {
  description = "Target ALB requests per minute per backend task (second scaling signal: the backend is mostly I/O-bound)."
  type        = number
  default     = 1200

  validation {
    condition     = var.backend_requests_per_target >= 50
    error_message = "backend_requests_per_target must be at least 50."
  }
}

variable "container_insights" {
  description = "ECS Container Insights mode for the cluster."
  type        = string
  default     = "enabled"

  validation {
    condition     = contains(["enabled", "enhanced", "disabled"], var.container_insights)
    error_message = "container_insights must be enabled, enhanced or disabled."
  }
}

# --- Backend settings that depend on the infrastructure (.env.example names) -----------

variable "backend_db_max_connections" {
  description = "DB_MAX_CONEXIONES: PostgreSQL connections per backend task."
  type        = number
  default     = 20

  validation {
    condition     = var.backend_db_max_connections >= 2 && var.backend_db_max_connections <= 200
    error_message = "backend_db_max_connections must be between 2 and 200."
  }
}

variable "backend_http_body_timeout_seconds" {
  description = "HTTP_CUERPO_SEG. Higher than the app default (15): unlike nginx, the ALB does not buffer request bodies, so a 12 MB KYC upload from a slow mobile link reaches the backend at the client's pace."
  type        = number
  default     = 60

  validation {
    condition     = var.backend_http_body_timeout_seconds >= 5 && var.backend_http_body_timeout_seconds <= 300
    error_message = "backend_http_body_timeout_seconds must be between 5 and 300."
  }
}

variable "backend_http_request_timeout_seconds" {
  description = "HTTP_PETICION_SEG: whole-request deadline, body included (503 TIEMPO_AGOTADO, the transaction rolls back)."
  type        = number
  default     = 75

  validation {
    condition     = var.backend_http_request_timeout_seconds >= 10 && var.backend_http_request_timeout_seconds <= 300 && var.backend_http_request_timeout_seconds >= var.backend_http_body_timeout_seconds
    error_message = "backend_http_request_timeout_seconds must be 10-300 and at least backend_http_body_timeout_seconds."
  }
}

variable "backend_http_max_connections" {
  description = "HTTP_MAX_CONEXIONES: concurrent connections per backend task."
  type        = number
  default     = 512

  validation {
    condition     = var.backend_http_max_connections >= 16 && var.backend_http_max_connections <= 16384
    error_message = "backend_http_max_connections must be between 16 and 16384."
  }
}

variable "backend_shutdown_timeout_seconds" {
  description = "HTTP_APAGADO_SEG: graceful shutdown deadline. The container stopTimeout is 10 s longer (Fargate allows at most 120 s)."
  type        = number
  default     = 20

  validation {
    condition     = var.backend_shutdown_timeout_seconds >= 1 && var.backend_shutdown_timeout_seconds <= 110
    error_message = "backend_shutdown_timeout_seconds must be between 1 and 110."
  }
}

variable "backend_rust_log" {
  description = "RUST_LOG filter for the backend and the migrate task."
  type        = string
  default     = "info"

  validation {
    condition     = can(regex("^[A-Za-z0-9_=,:.-]+$", var.backend_rust_log))
    error_message = "backend_rust_log must be a tracing EnvFilter directive such as info or info,synthetic_capital=debug."
  }
}

variable "jwt_ttl_minutes" {
  description = "JWT_TTL_MINUTOS: lifetime of issued tokens."
  type        = number
  default     = 60

  validation {
    condition     = var.jwt_ttl_minutes >= 5 && var.jwt_ttl_minutes <= 1440
    error_message = "jwt_ttl_minutes must be between 5 and 1440."
  }
}

variable "app_environment" {
  description = "Non-secret business settings from .env.example passed to both the backend service and the migrate task (bootstrap runs there): CLABE_PREFIJO, SPEI_PREFIJO_RASTREO, RESERVA_*, CAJA_*, KYC_SCORE_*, SPEI_*/CORTE_*/AML_* tuning. Secrets, database URLs and the settings managed by this configuration are rejected."
  type        = map(string)
  default     = {}

  validation {
    condition = alltrue([
      for k in keys(var.app_environment) :
      can(regex("^[A-Z][A-Z0-9_]*$", k))
      && !can(regex("(SECRET|PASSWORD|TOKEN|DATABASE_URL|_FILE$)", k))
      && !contains([
        "BIND_ADDR", "DB_MAX_CONEXIONES", "JWT_TTL_MINUTOS", "FRONTEND_ORIGIN", "RUST_LOG",
        "MIGRAR_AL_INICIO", "ARRANQUE_AL_INICIO", "MODO_DEMO", "HTTP_PROXY_CONFIABLE",
        "HTTP_CABECERAS_SEG", "HTTP_CUERPO_SEG", "HTTP_PETICION_SEG", "HTTP_MAX_CONEXIONES",
        "HTTP_APAGADO_SEG", "DB_APP_USUARIO", "BOOTSTRAP_OPERADOR_EMAIL",
        "BOOTSTRAP_CUMPLIMIENTO_EMAILS", "PGSSLROOTCERT", "PGSSLMODE", "PGHOST", "PGUSER",
      ], k)
    ])
    error_message = "app_environment keys must be UPPER_SNAKE_CASE, must not look like secrets or database URLs, and must not override settings managed by this configuration."
  }
}

variable "bootstrap_operator_email" {
  description = "BOOTSTRAP_OPERADOR_EMAIL: first operator, created by the migrate task if missing. Set: a secret for BOOTSTRAP_OPERADOR_PASSWORD is created (populate it before running the migrate task). Null: provision operators out of band."
  type        = string
  default     = null

  validation {
    condition     = var.bootstrap_operator_email == null || can(regex("^[^@\\s]+@[^@\\s]+\\.[^@\\s]+$", var.bootstrap_operator_email))
    error_message = "bootstrap_operator_email must be an email address."
  }
}

variable "enable_previous_webhook_secrets" {
  description = "Also inject SPEI_WEBHOOK_SECRETO_ANTERIOR and KYC_WEBHOOK_SECRETO_ANTERIOR (secrets created empty; populate them before the next deployment). Enable during a webhook secret rotation."
  type        = bool
  default     = false
}

variable "backend_egress_cidrs" {
  description = "IPv4 CIDR blocks the backend may reach on 443 through the NAT gateways (SPEI and KYC provider APIs). Narrow it to the providers' published ranges; [] removes internet egress entirely (the current backend makes no outbound calls). AWS APIs go through VPC endpoints and are not affected."
  type        = list(string)
  default     = ["0.0.0.0/0"]

  validation {
    condition     = alltrue([for c in var.backend_egress_cidrs : can(cidrnetmask(c))])
    error_message = "backend_egress_cidrs must contain IPv4 CIDR blocks."
  }
}

# -----------------------------------------------------------------------------------------
# Database (RDS for PostgreSQL 16 + RDS Proxy)
# -----------------------------------------------------------------------------------------

variable "db_engine_version" {
  description = "PostgreSQL engine version: a 16.x minor, or 16 for the default minor. Minor upgrades are applied automatically in the maintenance window."
  type        = string
  default     = "16"

  validation {
    condition     = can(regex("^16(\\.[0-9]+)?$", var.db_engine_version))
    error_message = "db_engine_version must be 16 or 16.<minor>."
  }
}

variable "db_instance_class" {
  description = "RDS instance class. Check availability in the chosen region."
  type        = string
  default     = "db.m7g.large"

  validation {
    condition     = can(regex("^db\\.[a-z0-9]+\\.[a-z0-9]+$", var.db_instance_class))
    error_message = "db_instance_class must look like db.m7g.large."
  }
}

variable "db_allocated_storage" {
  description = "Initial gp3 storage in GiB."
  type        = number
  default     = 100

  validation {
    condition     = var.db_allocated_storage >= 20 && var.db_allocated_storage <= 65536
    error_message = "db_allocated_storage must be between 20 and 65536 GiB."
  }
}

variable "db_max_allocated_storage" {
  description = "Storage autoscaling ceiling in GiB."
  type        = number
  default     = 1000

  validation {
    condition     = var.db_max_allocated_storage <= 65536 && var.db_max_allocated_storage > var.db_allocated_storage
    error_message = "db_max_allocated_storage must exceed db_allocated_storage and be at most 65536 GiB."
  }
}

variable "db_multi_az" {
  description = "Multi-AZ deployment (synchronous standby in another zone)."
  type        = bool
  default     = true
}

variable "db_name" {
  description = "Database name."
  type        = string
  default     = "synthetic_capital"

  validation {
    condition     = can(regex("^[a-z][a-z0-9_]{0,62}$", var.db_name))
    error_message = "db_name must start with a letter and contain only lowercase letters, digits and underscores (max 63)."
  }
}

variable "db_master_username" {
  description = "RDS master user, which is also the schema owner (DATABASE_URL_PROPIETARIO). Its password is generated, stored and rotated by RDS in Secrets Manager; only the migrate task reads it."
  type        = string
  default     = "sc_owner"

  validation {
    condition     = can(regex("^[a-z][a-z0-9_]{0,15}$", var.db_master_username)) && !contains(["postgres", "admin", "rdsadmin", "rds_superuser", "public", "user"], var.db_master_username)
    error_message = "db_master_username must be 1-16 lowercase letters, digits or underscores, start with a letter, and not be a reserved name."
  }
}

variable "db_app_username" {
  description = "Application login (DB_APP_USUARIO) that the migrate task creates or updates; the service connects as it through RDS Proxy."
  type        = string
  default     = "sc_app"

  validation {
    condition     = can(regex("^[a-z][a-z0-9_]{0,62}$", var.db_app_username)) && var.db_app_username != var.db_master_username
    error_message = "db_app_username must be a lowercase identifier different from db_master_username."
  }
}

variable "db_backup_retention_days" {
  description = "Automated backup retention (also the point-in-time recovery window)."
  type        = number
  default     = 35

  validation {
    condition     = var.db_backup_retention_days >= 7 && var.db_backup_retention_days <= 35
    error_message = "db_backup_retention_days must be between 7 and 35."
  }
}

variable "db_backup_window" {
  description = "Daily backup window (UTC). Default: 01:00-02:00 in Mexico City."
  type        = string
  default     = "07:00-08:00"

  validation {
    condition     = can(regex("^([01][0-9]|2[0-3]):[0-5][0-9]-([01][0-9]|2[0-3]):[0-5][0-9]$", var.db_backup_window))
    error_message = "db_backup_window must be hh24:mi-hh24:mi."
  }
}

variable "db_maintenance_window" {
  description = "Weekly maintenance window (UTC); must not overlap the backup window."
  type        = string
  default     = "sun:08:30-sun:09:30"

  validation {
    condition     = can(regex("^(mon|tue|wed|thu|fri|sat|sun):([01][0-9]|2[0-3]):[0-5][0-9]-(mon|tue|wed|thu|fri|sat|sun):([01][0-9]|2[0-3]):[0-5][0-9]$", var.db_maintenance_window))
    error_message = "db_maintenance_window must be ddd:hh24:mi-ddd:hh24:mi."
  }
}

variable "db_deletion_protection" {
  description = "Protect the database from deletion."
  type        = bool
  default     = true
}

variable "db_apply_immediately" {
  description = "Apply database modifications immediately instead of in the maintenance window."
  type        = bool
  default     = false
}

variable "db_performance_insights_retention_days" {
  description = "Performance Insights retention: 7 (free tier), a multiple of 31 up to 713, or 731."
  type        = number
  default     = 7

  validation {
    condition     = var.db_performance_insights_retention_days == 7 || var.db_performance_insights_retention_days == 731 || (var.db_performance_insights_retention_days % 31 == 0 && var.db_performance_insights_retention_days >= 31 && var.db_performance_insights_retention_days <= 713)
    error_message = "db_performance_insights_retention_days must be 7, 731 or a multiple of 31 between 31 and 713."
  }
}

variable "db_monitoring_interval" {
  description = "Enhanced Monitoring interval in seconds."
  type        = number
  default     = 15

  validation {
    condition     = contains([1, 5, 10, 15, 30, 60], var.db_monitoring_interval)
    error_message = "db_monitoring_interval must be 1, 5, 10, 15, 30 or 60."
  }
}

variable "db_log_min_duration_ms" {
  description = "log_min_duration_statement: log statements slower than this (ms; -1 disables). Bind parameters are never logged (log_parameter_max_length=0), so customer data stays out of the logs."
  type        = number
  default     = 1000

  validation {
    condition     = var.db_log_min_duration_ms == -1 || (var.db_log_min_duration_ms >= 100 && var.db_log_min_duration_ms <= 600000)
    error_message = "db_log_min_duration_ms must be -1 or between 100 and 600000."
  }
}

variable "db_proxy_max_connections_percent" {
  description = "Share of the database's max_connections that RDS Proxy may open."
  type        = number
  default     = 90

  validation {
    condition     = var.db_proxy_max_connections_percent >= 10 && var.db_proxy_max_connections_percent <= 100
    error_message = "db_proxy_max_connections_percent must be between 10 and 100."
  }
}

variable "db_proxy_idle_client_timeout" {
  description = "Seconds an idle client connection may stay open in RDS Proxy (sqlx recycles pooled connections well before the default 1800)."
  type        = number
  default     = 1800

  validation {
    condition     = var.db_proxy_idle_client_timeout >= 60 && var.db_proxy_idle_client_timeout <= 28800
    error_message = "db_proxy_idle_client_timeout must be between 60 and 28800."
  }
}

variable "rds_ca_bundle_pem" {
  description = "PEM bundle of the RDS certificate authorities for the region, passed to the migrate task as PGSSLROOTCERT. Null: downloaded at plan time from https://truststore.pki.rds.amazonaws.com/<region>/<region>-bundle.pem. Set it in pipelines without internet access."
  type        = string
  default     = null

  validation {
    condition     = var.rds_ca_bundle_pem == null || can(regex("-----BEGIN CERTIFICATE-----", var.rds_ca_bundle_pem))
    error_message = "rds_ca_bundle_pem must contain PEM certificates."
  }
}

# -----------------------------------------------------------------------------------------
# Keys, secrets and logs
# -----------------------------------------------------------------------------------------

variable "kms_deletion_window_days" {
  description = "Waiting period before a scheduled KMS key deletion takes effect."
  type        = number
  default     = 30

  validation {
    condition     = var.kms_deletion_window_days >= 7 && var.kms_deletion_window_days <= 30
    error_message = "kms_deletion_window_days must be between 7 and 30."
  }
}

variable "secret_recovery_window_days" {
  description = "Days a deleted Secrets Manager secret can still be restored."
  type        = number
  default     = 30

  validation {
    condition     = var.secret_recovery_window_days >= 7 && var.secret_recovery_window_days <= 30
    error_message = "secret_recovery_window_days must be between 7 and 30."
  }
}

variable "log_retention_days" {
  description = "CloudWatch Logs retention for application, VPC flow, database, proxy and WAF logs. Archive to S3 for multi-year regulatory retention."
  type        = number
  default     = 365

  validation {
    condition     = contains([1, 3, 5, 7, 14, 30, 60, 90, 120, 150, 180, 365, 400, 545, 731, 1096, 1827, 2192, 2557, 2922, 3288, 3653], var.log_retention_days)
    error_message = "log_retention_days must be a CloudWatch Logs retention value (1, 3, 5, 7, 14, 30, 60, 90, 120, 150, 180, 365, 400, 545, 731, 1096, 1827, 2192, 2557, 2922, 3288 or 3653)."
  }
}

variable "log_group_deletion_protection" {
  description = "Protect the CloudWatch log groups from deletion (audit evidence)."
  type        = bool
  default     = true
}

variable "alb_log_retention_days" {
  description = "Days to keep ALB access and connection logs in S3."
  type        = number
  default     = 400

  validation {
    condition     = var.alb_log_retention_days >= 30 && var.alb_log_retention_days <= 3653
    error_message = "alb_log_retention_days must be between 30 and 3653."
  }
}
