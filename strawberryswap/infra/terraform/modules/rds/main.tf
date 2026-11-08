# Off-chain state for the Automator relayers: oracle price history, signed intents / limit-order book,
# batch records. PostgreSQL on a single db.t3.micro (Free Tier: 750 h/month, 20 GB, 12 months).
# Not publicly accessible, no route to the internet, reachable only from the bot security group.
resource "random_password" "master" {
  length           = 32
  special          = true
  override_special = "!#$%^&*()-_=+[]{}" # no '/', '@', '"', ' ' (RDS-forbidden)
}

# Secret goes to SSM Parameter Store (standard tier SecureString = free). Secrets Manager is NOT free.
resource "aws_ssm_parameter" "db_password" {
  name        = "/${var.name}/automator/db_password"
  type        = "SecureString"
  value       = random_password.master.result
  description = "RDS master password for the Automator database"
}

resource "aws_db_subnet_group" "this" {
  name       = "${var.name}-automator"
  subnet_ids = var.private_subnet_ids
}

resource "aws_security_group" "db" {
  name_prefix = "${var.name}-rds-"
  description = "PostgreSQL from Automator instances only"
  vpc_id      = var.vpc_id
  lifecycle { create_before_destroy = true }
}

resource "aws_vpc_security_group_ingress_rule" "from_bots" {
  security_group_id            = aws_security_group.db.id
  referenced_security_group_id = var.client_security_group_id
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
  description                  = "Automator -> Postgres"
}

# Force TLS for every connection.
resource "aws_db_parameter_group" "this" {
  name_prefix = "${var.name}-pg16-"
  family      = "postgres16"
  parameter {
    name  = "rds.force_ssl"
    value = "1"
  }
  lifecycle { create_before_destroy = true }
}

resource "aws_db_instance" "this" {
  identifier     = "${var.name}-automator"
  engine         = "postgres"
  engine_version = var.engine_version
  instance_class = var.instance_class # db.t3.micro | db.t4g.micro are Free Tier eligible

  allocated_storage = 20 # Free Tier cap
  storage_type      = "gp2"
  storage_encrypted = true

  db_name  = "automator"
  username = "automator_admin"
  password = random_password.master.result
  port     = 5432

  db_subnet_group_name   = aws_db_subnet_group.this.name
  vpc_security_group_ids = [aws_security_group.db.id]
  parameter_group_name   = aws_db_parameter_group.this.name
  publicly_accessible    = false
  multi_az               = false # Multi-AZ doubles cost and leaves Free Tier

  iam_database_authentication_enabled = true
  backup_retention_period             = 1
  auto_minor_version_upgrade          = true
  copy_tags_to_snapshot               = true

  deletion_protection       = var.deletion_protection
  skip_final_snapshot       = !var.deletion_protection
  final_snapshot_identifier = var.deletion_protection ? "${var.name}-automator-final" : null
}
