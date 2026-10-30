output "app_url" {
  description = "Public URL of the bank (also the frontend's NEXT_PUBLIC_API_URL build argument)."
  value       = local.public_url
}

output "alb_dns_name" {
  description = "ALB DNS name: target of the CNAME/alias record for domain_name when Route 53 is not used."
  value       = aws_lb.this.dns_name
}

output "alb_zone_id" {
  description = "Hosted zone ID of the ALB (for alias records)."
  value       = aws_lb.this.zone_id
}

output "acm_validation_records" {
  description = "DNS records that validate the requested ACM certificate (create them at the DNS provider when route53_zone_id is null). Empty when acm_certificate_arn is set."
  value       = [for v in values(local.acm_validation_records) : v]
}

output "egress_public_ips" {
  description = "Public addresses of the NAT gateways: the backend's source IPs towards the SPEI and KYC providers (give them to the providers for their allowlists)."
  value       = [for e in aws_eip.nat : e.public_ip]
}

output "ecr_repository_urls" {
  description = "ECR repositories to push the images to (immutable tags)."
  value       = { for k, r in aws_ecr_repository.this : k => r.repository_url }
}

output "ecs_cluster_name" {
  description = "ECS cluster name."
  value       = aws_ecs_cluster.this.name
}

output "ecs_service_names" {
  description = "ECS service names."
  value = {
    backend  = aws_ecs_service.backend.name
    frontend = aws_ecs_service.frontend.name
  }
}

output "migrate_task_definition_arn" {
  description = "Task definition of the one-off migrate task (`synthetic-capital migrar`)."
  value       = aws_ecs_task_definition.migrate.arn
}

output "migrate_run_task_command" {
  description = "Runs the migrate task once and prints its task ARN. Then: aws ecs wait tasks-stopped --cluster <cluster> --tasks <arn>, and check the exit code with aws ecs describe-tasks (tasks[0].containers[0].exitCode must be 0)."
  value = join(" ", [
    "aws ecs run-task",
    "--region ${var.region}",
    "--cluster ${aws_ecs_cluster.this.name}",
    "--launch-type FARGATE",
    "--platform-version LATEST",
    "--task-definition ${aws_ecs_task_definition.migrate.arn}",
    "--started-by migrate",
    "--enable-ecs-managed-tags --propagate-tags TASK_DEFINITION",
    "--network-configuration 'awsvpcConfiguration={subnets=[${join(",", [for s in aws_subnet.app : s.id])}],securityGroups=[${aws_security_group.migrate.id}],assignPublicIp=DISABLED}'",
    "--query 'tasks[0].taskArn' --output text",
  ])
}

output "secrets_to_populate" {
  description = "Secrets created empty: put their values (out of band, never through Terraform) before the services scale above zero or the migrate task runs."
  value = merge(
    { for name, s in aws_secretsmanager_secret.backend : name => s.arn },
    { for s in aws_secretsmanager_secret.bootstrap_operator_password : "BOOTSTRAP_OPERADOR_PASSWORD" => s.arn },
  )
}

output "generated_secret_arns" {
  description = "Secrets whose values Terraform generates (they are also in the state)."
  value = {
    db_app_credentials   = aws_secretsmanager_secret.db_app.arn
    backend_database_url = aws_secretsmanager_secret.backend_database_url.arn
  }
}

output "db_master_user_secret_arn" {
  description = "RDS-managed secret of the master user (schema owner); rotated by RDS, read only by the migrate task."
  value       = aws_db_instance.this.master_user_secret[0].secret_arn
}

output "db_instance_identifier" {
  description = "RDS instance identifier."
  value       = aws_db_instance.this.identifier
}

output "db_instance_address" {
  description = "RDS instance endpoint (reachable only from the migrate task's security group)."
  value       = aws_db_instance.this.address
}

output "db_proxy_endpoint" {
  description = "RDS Proxy endpoint used by the backend (DATABASE_URL)."
  value       = aws_db_proxy.this.endpoint
}

output "waf_web_acl_arn" {
  description = "WAF web ACL attached to the ALB."
  value       = aws_wafv2_web_acl.this.arn
}

output "log_group_names" {
  description = "CloudWatch log groups."
  value = merge(
    { for k, g in aws_cloudwatch_log_group.ecs : "ecs_${k}" => g.name },
    { for k, g in aws_cloudwatch_log_group.rds : "rds_${k}" => g.name },
    {
      vpc_flow  = aws_cloudwatch_log_group.vpc_flow.name
      rds_proxy = aws_cloudwatch_log_group.rds_proxy.name
      waf       = aws_cloudwatch_log_group.waf.name
    },
  )
}

output "alb_logs_bucket" {
  description = "S3 bucket with the ALB access (access/) and connection (connection/) logs."
  value       = aws_s3_bucket.alb_logs.id
}

output "kms_key_arns" {
  description = "Customer managed KMS keys."
  value = {
    rds     = aws_kms_key.rds.arn
    secrets = aws_kms_key.secrets.arn
    logs    = aws_kms_key.logs.arn
    ecr     = aws_kms_key.ecr.arn
  }
}

output "vpc_id" {
  description = "VPC ID."
  value       = aws_vpc.this.id
}

output "subnet_ids" {
  description = "Subnet IDs per tier."
  value = {
    public = [for s in aws_subnet.public : s.id]
    app    = [for s in aws_subnet.app : s.id]
    data   = [for s in aws_subnet.data : s.id]
  }
}

output "security_group_ids" {
  description = "Security group IDs."
  value = {
    alb       = aws_security_group.alb.id
    backend   = aws_security_group.backend.id
    frontend  = aws_security_group.frontend.id
    migrate   = aws_security_group.migrate.id
    db_proxy  = aws_security_group.db_proxy.id
    db        = aws_security_group.db.id
    endpoints = aws_security_group.endpoints.id
  }
}
