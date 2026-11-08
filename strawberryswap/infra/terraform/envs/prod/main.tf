module "network" {
  source = "../../modules/network"
  name   = var.name
}

module "metadata" {
  source = "../../modules/s3-ipfs-pin"
  name   = var.name
}

module "database" {
  source                   = "../../modules/rds"
  name                     = var.name
  vpc_id                   = module.network.vpc_id
  private_subnet_ids       = module.network.private_subnet_ids
  client_security_group_id = module.network.bots_security_group_id
  instance_class           = var.db_instance_class
  deletion_protection      = var.db_deletion_protection
}

module "automators" {
  source                     = "../../modules/ec2-bots"
  name                       = var.name
  public_subnet_ids          = module.network.public_subnet_ids
  security_group_id          = module.network.bots_security_group_id
  instance_type              = var.instance_type
  instance_count             = var.instance_count
  allow_over_free_tier       = var.allow_over_free_tier
  db_host                    = module.database.address
  db_port                    = module.database.port
  db_name                    = module.database.db_name
  db_user                    = module.database.username
  db_password_parameter_name = module.database.password_parameter_name
  metadata_bucket_name       = module.metadata.bucket_name
  metadata_bucket_arn        = module.metadata.bucket_arn
  metadata_kms_key_arn       = module.metadata.kms_key_arn
}

# Free Tier guard rail: email when actual spend passes 80 % or forecast passes 100 % of the budget.
resource "aws_budgets_budget" "monthly" {
  name         = "${var.name}-monthly"
  budget_type  = "COST"
  limit_amount = tostring(var.monthly_budget_usd)
  limit_unit   = "USD"
  time_unit    = "MONTHLY"

  notification {
    comparison_operator        = "GREATER_THAN"
    threshold                  = 80
    threshold_type             = "PERCENTAGE"
    notification_type          = "ACTUAL"
    subscriber_email_addresses = [var.alert_email]
  }
  notification {
    comparison_operator        = "GREATER_THAN"
    threshold                  = 100
    threshold_type             = "PERCENTAGE"
    notification_type          = "FORECASTED"
    subscriber_email_addresses = [var.alert_email]
  }
}
