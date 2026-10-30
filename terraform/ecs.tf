# ECS on Fargate: backend and frontend services behind the ALB, plus the one-off migrate
# task definition (run by hand or by CI with `aws ecs run-task`; see the
# migrate_run_task_command output).
#
# Container hardening, all three containers:
#   readonlyRootFilesystem, user 65532:65532, capabilities drop ALL, initProcessEnabled
#   (tini as PID 1: signals forwarded, zombies reaped), no privileged mode, awsvpc
#   networking in private subnets without public IPs, CPU/memory fixed per task.
#   Fargate rejects dockerSecurityOptions, so "no-new-privileges" cannot be set
#   explicitly. With every capability dropped from the bounding set, a non-root user and
#   distroless images (no setuid binaries, no shell), there is no privilege to gain.
#
# No task role: the application calls no AWS API, so its containers get no AWS
# credentials at all. Each task definition has its own execution role (used by the
# Fargate agent to pull the image, write logs and inject secrets); only the migrate
# task's role can read the schema owner's secret.

resource "aws_ecs_cluster" "this" {
  #checkov:skip=CKV_AWS_224:ECS Exec is disabled on every service and the distroless images have no shell, so there are no exec sessions to log or encrypt.
  name = local.name

  setting {
    name  = "containerInsights"
    value = var.container_insights
  }

  tags = { Name = local.name }
}

resource "aws_ecs_cluster_capacity_providers" "this" {
  cluster_name       = aws_ecs_cluster.this.name
  capacity_providers = ["FARGATE"]

  default_capacity_provider_strategy {
    capacity_provider = "FARGATE"
    weight            = 1
  }
}

# -----------------------------------------------------------------------------------------
# Execution roles
# -----------------------------------------------------------------------------------------

data "aws_iam_policy_document" "ecs_tasks_assume" {
  statement {
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["ecs-tasks.amazonaws.com"]
    }

    condition {
      test     = "StringEquals"
      variable = "aws:SourceAccount"
      values   = [local.account_id]
    }

    condition {
      test     = "ArnLike"
      variable = "aws:SourceArn"
      values   = ["arn:${local.partition}:ecs:${var.region}:${local.account_id}:*"]
    }
  }
}

locals {
  ecs_components = {
    backend  = { repository = "backend" }
    frontend = { repository = "frontend" }
    migrate  = { repository = "backend" }
  }

  # Secrets each execution role may read (and nothing else).
  ecs_secret_arns = {
    backend = concat(
      [aws_secretsmanager_secret.backend_database_url.arn],
      [for s in aws_secretsmanager_secret.backend : s.arn],
    )
    frontend = []
    migrate = concat(
      [aws_db_instance.this.master_user_secret[0].secret_arn, aws_secretsmanager_secret.db_app.arn],
      aws_secretsmanager_secret.bootstrap_operator_password[*].arn,
      # Only during a rotation of the TOTP encryption key: migrate re-encrypts.
      [for k in local.migrate_mfa_keys : aws_secretsmanager_secret.backend[k].arn],
    )
  }
}

resource "aws_iam_role" "ecs_execution" {
  for_each = local.ecs_components

  name               = "${local.name}-${each.key}-execution"
  assume_role_policy = data.aws_iam_policy_document.ecs_tasks_assume.json

  tags = { Name = "${local.name}-${each.key}-execution", Component = each.key }
}

data "aws_iam_policy_document" "ecs_execution" {
  for_each = local.ecs_components

  statement {
    sid       = "EcrAuthorizationToken"
    actions   = ["ecr:GetAuthorizationToken"] # not resource-scoped by AWS
    resources = ["*"]
  }

  statement {
    sid       = "PullOwnImage"
    actions   = ["ecr:BatchCheckLayerAvailability", "ecr:GetDownloadUrlForLayer", "ecr:BatchGetImage"]
    resources = [aws_ecr_repository.this[each.value.repository].arn]
  }

  statement {
    sid       = "WriteOwnLogs"
    actions   = ["logs:CreateLogStream", "logs:PutLogEvents"]
    resources = ["${aws_cloudwatch_log_group.ecs[each.key].arn}:*"]
  }

  dynamic "statement" {
    for_each = length(local.ecs_secret_arns[each.key]) > 0 ? [1] : []
    content {
      sid       = "ReadOwnSecrets"
      actions   = ["secretsmanager:GetSecretValue"]
      resources = local.ecs_secret_arns[each.key]
    }
  }

  dynamic "statement" {
    for_each = length(local.ecs_secret_arns[each.key]) > 0 ? [1] : []
    content {
      sid       = "DecryptSecretsThroughSecretsManager"
      actions   = ["kms:Decrypt"]
      resources = [aws_kms_key.secrets.arn]

      condition {
        test     = "StringEquals"
        variable = "kms:ViaService"
        values   = ["secretsmanager.${var.region}.amazonaws.com"]
      }
    }
  }
}

resource "aws_iam_role_policy" "ecs_execution" {
  for_each = local.ecs_components

  name   = "pull-log-secrets"
  role   = aws_iam_role.ecs_execution[each.key].id
  policy = data.aws_iam_policy_document.ecs_execution[each.key].json
}

# -----------------------------------------------------------------------------------------
# Container definitions
# -----------------------------------------------------------------------------------------

locals {
  container_hardening = {
    readonlyRootFilesystem = true
    user                   = "65532:65532"
    privileged             = false
    linuxParameters = {
      initProcessEnabled = true
      capabilities       = { drop = ["ALL"] }
    }
    ulimits = [{ name = "nofile", softLimit = 65535, hardLimit = 65535 }]
  }

  # Backend service (.env.example names). Business settings come from app_environment;
  # the keys below are managed here and cannot be overridden (see its validation).
  backend_environment = merge(var.app_environment, {
    BIND_ADDR                = "0.0.0.0:8080"
    DB_MAX_CONEXIONES        = tostring(var.backend_db_max_connections)
    JWT_TTL_MINUTOS          = tostring(var.jwt_ttl_minutes)
    JWT_TTL_PERSONAL_MINUTOS = tostring(var.jwt_ttl_staff_minutes)
    FRONTEND_ORIGIN          = local.public_url
    RUST_LOG                 = var.backend_rust_log
    MODO_DEMO                = "false"
    MIGRAR_AL_INICIO         = "false" # migrations run only in the migrate task
    ARRANQUE_AL_INICIO       = "false" # bootstrap runs only in the migrate task
    HTTP_PROXY_CONFIABLE     = "true"  # client IP = last X-Forwarded-For hop, appended by the ALB
    # Idle keep-alive limit above the ALB idle timeout, so the ALB never sends a request
    # on a connection the backend has just closed (502). Slow-header attacks stop at the
    # ALB, which only forwards complete request heads.
    HTTP_CABECERAS_SEG  = tostring(var.alb_idle_timeout_seconds + 15)
    HTTP_CUERPO_SEG     = tostring(var.backend_http_body_timeout_seconds)
    HTTP_PETICION_SEG   = tostring(var.backend_http_request_timeout_seconds)
    HTTP_MAX_CONEXIONES = tostring(var.backend_http_max_connections)
    HTTP_APAGADO_SEG    = tostring(var.backend_shutdown_timeout_seconds)
  })

  backend_secrets = merge(
    { DATABASE_URL = aws_secretsmanager_secret.backend_database_url.arn },
    { for name, s in aws_secretsmanager_secret.backend : name => s.arn },
  )

  # Migrate task: `synthetic-capital migrar` (migrations, bootstrap, application login).
  #   DATABASE_URL_PROPIETARIO has no password on purpose: RDS generates and rotates the
  #   owner password, so Terraform cannot embed it. ECS injects it as PGPASSWORD from the
  #   RDS-managed secret, and sqlx fills a password-less URL from PGPASSWORD.
  #   Direct to the instance endpoint (not the proxy), TLS verify-full against the
  #   regional RDS CA bundle in PGSSLROOTCERT.
  migrate_environment = merge(
    var.app_environment,
    {
      DATABASE_URL_PROPIETARIO = "postgres://${var.db_master_username}@${aws_db_instance.this.address}:${aws_db_instance.this.port}/${var.db_name}?sslmode=verify-full"
      PGSSLROOTCERT            = local.rds_ca_bundle_pem
      DB_APP_USUARIO           = var.db_app_username
      RUST_LOG                 = var.backend_rust_log
      MODO_DEMO                = "false"
    },
    var.bootstrap_operator_email == null ? {} : { BOOTSTRAP_OPERADOR_EMAIL = var.bootstrap_operator_email },
  )

  # With both TOTP encryption keys, migrate re-encrypts every secret with the current one.
  migrate_mfa_keys = var.enable_previous_mfa_key ? ["MFA_CLAVE_CIFRADO", "MFA_CLAVE_CIFRADO_ANTERIOR"] : []

  migrate_secrets = merge(
    {
      PGPASSWORD      = "${aws_db_instance.this.master_user_secret[0].secret_arn}:password::"
      DB_APP_PASSWORD = "${aws_secretsmanager_secret.db_app.arn}:password::"
    },
    { for s in aws_secretsmanager_secret.bootstrap_operator_password : "BOOTSTRAP_OPERADOR_PASSWORD" => s.arn },
    { for k in local.migrate_mfa_keys : k => aws_secretsmanager_secret.backend[k].arn },
  )

  frontend_environment = {
    NODE_ENV                = "production"
    NEXT_TELEMETRY_DISABLED = "1"
    HOSTNAME                = "0.0.0.0"
    PORT                    = "3000"
  }

  backend_container = merge(local.container_hardening, {
    name         = "backend"
    image        = local.backend_image
    essential    = true
    portMappings = [{ name = "http", containerPort = 8080, protocol = "tcp" }]
    environment  = [for k in sort(keys(local.backend_environment)) : { name = k, value = local.backend_environment[k] }]
    secrets      = [for k in sort(keys(local.backend_secrets)) : { name = k, valueFrom = local.backend_secrets[k] }]
    mountPoints  = []
    volumesFrom  = []
    healthCheck = {
      command     = ["CMD", "/usr/local/bin/synthetic-capital", "salud", "vivo"]
      interval    = 15
      timeout     = 5
      retries     = 3
      startPeriod = 30
    }
    stopTimeout = var.backend_shutdown_timeout_seconds + 10
    logConfiguration = {
      logDriver = "awslogs"
      options = {
        awslogs-group         = aws_cloudwatch_log_group.ecs["backend"].name
        awslogs-region        = var.region
        awslogs-stream-prefix = "backend"
        mode                  = "non-blocking"
        max-buffer-size       = "25m"
      }
    }
  })

  migrate_container = merge(local.container_hardening, {
    name        = "migrate"
    image       = local.migrate_image
    essential   = true
    command     = ["migrar"]
    environment = [for k in sort(keys(local.migrate_environment)) : { name = k, value = local.migrate_environment[k] }]
    secrets     = [for k in sort(keys(local.migrate_secrets)) : { name = k, valueFrom = local.migrate_secrets[k] }]
    mountPoints = []
    volumesFrom = []
    stopTimeout = 30
    logConfiguration = {
      logDriver = "awslogs"
      options = {
        awslogs-group         = aws_cloudwatch_log_group.ecs["migrate"].name
        awslogs-region        = var.region
        awslogs-stream-prefix = "migrate"
        mode                  = "blocking" # a short job: keep every line
      }
    }
  })

  frontend_container = merge(local.container_hardening, {
    name         = "frontend"
    image        = local.frontend_image
    essential    = true
    portMappings = [{ name = "http", containerPort = 3000, protocol = "tcp" }]
    environment  = [for k in sort(keys(local.frontend_environment)) : { name = k, value = local.frontend_environment[k] }]
    # The only writable path: Next.js' cache, on task ephemeral storage (Fargate has no
    # tmpfs). The image must declare VOLUME /app/.next/cache owned by 65532, so Fargate
    # creates the mount with that ownership instead of root:root 0755.
    mountPoints = [{ sourceVolume = "next-cache", containerPath = "/app/.next/cache", readOnly = false }]
    volumesFrom = []
    healthCheck = {
      command     = ["CMD", "/nodejs/bin/node", "-e", "fetch('http://127.0.0.1:3000/',{signal:AbortSignal.timeout(4000)}).then(r=>process.exit(r.ok?0:1),()=>process.exit(1))"]
      interval    = 30
      timeout     = 5
      retries     = 3
      startPeriod = 20
    }
    stopTimeout = 30
    logConfiguration = {
      logDriver = "awslogs"
      options = {
        awslogs-group         = aws_cloudwatch_log_group.ecs["frontend"].name
        awslogs-region        = var.region
        awslogs-stream-prefix = "frontend"
        mode                  = "non-blocking"
        max-buffer-size       = "25m"
      }
    }
  })
}

# -----------------------------------------------------------------------------------------
# Task definitions
# -----------------------------------------------------------------------------------------

resource "aws_ecs_task_definition" "backend" {
  family                   = "${local.name}-backend"
  requires_compatibilities = ["FARGATE"]
  network_mode             = "awsvpc"
  cpu                      = tostring(var.backend_cpu)
  memory                   = tostring(var.backend_memory)
  execution_role_arn       = aws_iam_role.ecs_execution["backend"].arn
  container_definitions    = jsonencode([local.backend_container])

  runtime_platform {
    operating_system_family = "LINUX"
    cpu_architecture        = var.cpu_architecture
  }

  tags = { Name = "${local.name}-backend", Component = "backend" }
}

resource "aws_ecs_task_definition" "migrate" {
  family                   = "${local.name}-migrate"
  requires_compatibilities = ["FARGATE"]
  network_mode             = "awsvpc"
  cpu                      = tostring(var.migrate_cpu)
  memory                   = tostring(var.migrate_memory)
  execution_role_arn       = aws_iam_role.ecs_execution["migrate"].arn
  container_definitions    = jsonencode([local.migrate_container])

  runtime_platform {
    operating_system_family = "LINUX"
    cpu_architecture        = var.cpu_architecture
  }

  tags = { Name = "${local.name}-migrate", Component = "migrate" }
}

resource "aws_ecs_task_definition" "frontend" {
  family                   = "${local.name}-frontend"
  requires_compatibilities = ["FARGATE"]
  network_mode             = "awsvpc"
  cpu                      = tostring(var.frontend_cpu)
  memory                   = tostring(var.frontend_memory)
  execution_role_arn       = aws_iam_role.ecs_execution["frontend"].arn
  container_definitions    = jsonencode([local.frontend_container])

  # Bind mount on the task's ephemeral storage (encrypted by Fargate).
  volume {
    name = "next-cache"
  }

  runtime_platform {
    operating_system_family = "LINUX"
    cpu_architecture        = var.cpu_architecture
  }

  tags = { Name = "${local.name}-frontend", Component = "frontend" }
}

# -----------------------------------------------------------------------------------------
# Services
# -----------------------------------------------------------------------------------------

resource "aws_ecs_service" "backend" {
  name                              = "backend"
  cluster                           = aws_ecs_cluster.this.id
  task_definition                   = aws_ecs_task_definition.backend.arn
  desired_count                     = var.backend_min_tasks
  launch_type                       = "FARGATE"
  platform_version                  = "LATEST"
  enable_execute_command            = false
  enable_ecs_managed_tags           = true
  propagate_tags                    = "SERVICE"
  health_check_grace_period_seconds = 60
  availability_zone_rebalancing     = "ENABLED"

  deployment_minimum_healthy_percent = 100
  deployment_maximum_percent         = 200

  deployment_circuit_breaker {
    enable   = true
    rollback = true
  }

  network_configuration {
    subnets          = [for s in aws_subnet.app : s.id]
    security_groups  = [aws_security_group.backend.id]
    assign_public_ip = false
  }

  load_balancer {
    target_group_arn = aws_lb_target_group.backend.arn
    container_name   = "backend"
    container_port   = 8080
  }

  tags = { Name = "${local.name}-backend", Component = "backend" }

  # Autoscaling owns the task count after creation.
  lifecycle {
    ignore_changes = [desired_count]
  }

  depends_on = [
    aws_lb_listener_rule.backend,
    aws_iam_role_policy.ecs_execution,
    aws_secretsmanager_secret_version.backend_database_url,
    aws_db_proxy_target.this,
  ]
}

resource "aws_ecs_service" "frontend" {
  name                              = "frontend"
  cluster                           = aws_ecs_cluster.this.id
  task_definition                   = aws_ecs_task_definition.frontend.arn
  desired_count                     = var.frontend_min_tasks
  launch_type                       = "FARGATE"
  platform_version                  = "LATEST"
  enable_execute_command            = false
  enable_ecs_managed_tags           = true
  propagate_tags                    = "SERVICE"
  health_check_grace_period_seconds = 30
  availability_zone_rebalancing     = "ENABLED"

  deployment_minimum_healthy_percent = 100
  deployment_maximum_percent         = 200

  deployment_circuit_breaker {
    enable   = true
    rollback = true
  }

  network_configuration {
    subnets          = [for s in aws_subnet.app : s.id]
    security_groups  = [aws_security_group.frontend.id]
    assign_public_ip = false
  }

  load_balancer {
    target_group_arn = aws_lb_target_group.frontend.arn
    container_name   = "frontend"
    container_port   = 3000
  }

  tags = { Name = "${local.name}-frontend", Component = "frontend" }

  lifecycle {
    ignore_changes = [desired_count]
  }

  depends_on = [
    aws_lb_listener.https,
    aws_iam_role_policy.ecs_execution,
  ]
}

# -----------------------------------------------------------------------------------------
# Autoscaling (target tracking). Raising min_capacity above the current count scales the
# service out immediately, which is how the first deployment leaves zero tasks.
# -----------------------------------------------------------------------------------------

resource "aws_appautoscaling_target" "backend" {
  service_namespace  = "ecs"
  resource_id        = "service/${aws_ecs_cluster.this.name}/${aws_ecs_service.backend.name}"
  scalable_dimension = "ecs:service:DesiredCount"
  min_capacity       = var.backend_min_tasks
  max_capacity       = var.backend_max_tasks

  tags = { Name = "${local.name}-backend", Component = "backend" }
}

resource "aws_appautoscaling_policy" "backend_cpu" {
  name               = "${local.name}-backend-cpu"
  policy_type        = "TargetTrackingScaling"
  service_namespace  = aws_appautoscaling_target.backend.service_namespace
  resource_id        = aws_appautoscaling_target.backend.resource_id
  scalable_dimension = aws_appautoscaling_target.backend.scalable_dimension

  target_tracking_scaling_policy_configuration {
    target_value       = var.autoscaling_cpu_target
    scale_in_cooldown  = 300
    scale_out_cooldown = 60

    predefined_metric_specification {
      predefined_metric_type = "ECSServiceAverageCPUUtilization"
    }
  }
}

resource "aws_appautoscaling_policy" "backend_requests" {
  name               = "${local.name}-backend-requests"
  policy_type        = "TargetTrackingScaling"
  service_namespace  = aws_appautoscaling_target.backend.service_namespace
  resource_id        = aws_appautoscaling_target.backend.resource_id
  scalable_dimension = aws_appautoscaling_target.backend.scalable_dimension

  target_tracking_scaling_policy_configuration {
    target_value       = var.backend_requests_per_target
    scale_in_cooldown  = 300
    scale_out_cooldown = 60

    predefined_metric_specification {
      predefined_metric_type = "ALBRequestCountPerTarget"
      resource_label         = "${aws_lb.this.arn_suffix}/${aws_lb_target_group.backend.arn_suffix}"
    }
  }
}

resource "aws_appautoscaling_target" "frontend" {
  service_namespace  = "ecs"
  resource_id        = "service/${aws_ecs_cluster.this.name}/${aws_ecs_service.frontend.name}"
  scalable_dimension = "ecs:service:DesiredCount"
  min_capacity       = var.frontend_min_tasks
  max_capacity       = var.frontend_max_tasks

  tags = { Name = "${local.name}-frontend", Component = "frontend" }
}

resource "aws_appautoscaling_policy" "frontend_cpu" {
  name               = "${local.name}-frontend-cpu"
  policy_type        = "TargetTrackingScaling"
  service_namespace  = aws_appautoscaling_target.frontend.service_namespace
  resource_id        = aws_appautoscaling_target.frontend.resource_id
  scalable_dimension = aws_appautoscaling_target.frontend.scalable_dimension

  target_tracking_scaling_policy_configuration {
    target_value       = var.autoscaling_cpu_target
    scale_in_cooldown  = 300
    scale_out_cooldown = 60

    predefined_metric_specification {
      predefined_metric_type = "ECSServiceAverageCPUUtilization"
    }
  }
}
