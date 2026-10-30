# DBA access path: a bastion that only AWS Systems Manager Session Manager can reach.
# DEPLOYMENT.md, "Initial DBA Bootstrap & Admin Creation", is the runbook;
# scripts/db-tunnel.sh opens the tunnel.
#
#   DBA laptop --HTTPS + IAM--> Session Manager <--agent's outbound channel (ssmmessages
#   endpoint)-- bastion (app subnet, no public address, no inbound rule) --5432--> RDS
#
# - Zero inbound: the bastion security group has no ingress rule, the instance has no
#   public address and no key pair, and sshd is disabled. The SSM agent opens its own
#   outbound channel through the interface endpoints in vpc.tf.
# - Egress: HTTPS to the VPC endpoints and PostgreSQL to the RDS instance, nothing else, so
#   a port forward can reach only the database. The instance never needs the internet.
# - Who: the dba-tunnel IAM policy (attach it to the DBA's role or IAM Identity Center
#   permission set) allows AWS-StartPortForwardingSessionToRemoteHost on this instance and
#   nothing else. ssm:SessionDocumentAccessCheck refuses a session that names no document
#   (a shell) or any other document.
# - Audit: CloudTrail records StartSession (caller, instance, document, host and ports),
#   TerminateSession and each GetSecretValue on the master user secret. PostgreSQL logs
#   every connection, with the bastion's address as the source, and every DDL statement
#   (rds.tf parameter group, exported to CloudWatch Logs).
# - Stateless: it is recreated from the latest Amazon Linux 2023 AMI whenever AWS
#   publishes one (the plan shows the replacement), so patches arrive without package
#   egress. dba_bastion_enabled = false removes the instance, its endpoints, its database
#   rule and the policy, for example between maintenance windows.

locals {
  dba_bastion_zone     = coalesce(var.dba_bastion_availability_zone, local.azs[0])
  dba_session_document = "AWS-StartPortForwardingSessionToRemoteHost"
}

data "aws_ec2_instance_type" "dba_bastion" {
  count = var.dba_bastion_enabled ? 1 : 0

  instance_type = var.dba_bastion_instance_type
}

# The standard image, not the minimal one: only the standard image ships the SSM agent.
data "aws_ssm_parameter" "dba_bastion_ami" {
  count = var.dba_bastion_enabled ? 1 : 0

  name = format(
    "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-%s",
    contains(data.aws_ec2_instance_type.dba_bastion[0].supported_architectures, "arm64") ? "arm64" : "x86_64",
  )
}

# -----------------------------------------------------------------------------------------
# Instance
# -----------------------------------------------------------------------------------------

resource "aws_instance" "dba_bastion" {
  count = var.dba_bastion_enabled ? 1 : 0

  ami                         = data.aws_ssm_parameter.dba_bastion_ami[0].insecure_value
  instance_type               = var.dba_bastion_instance_type
  subnet_id                   = aws_subnet.app[local.dba_bastion_zone].id
  vpc_security_group_ids      = [aws_security_group.dba_bastion[0].id]
  associate_public_ip_address = false
  iam_instance_profile        = aws_iam_instance_profile.dba_bastion[0].name
  monitoring                  = true
  ebs_optimized               = true

  metadata_options {
    http_endpoint               = "enabled"
    http_tokens                 = "required" # IMDSv2 only
    http_put_response_hop_limit = 1
    instance_metadata_tags      = "disabled"
  }

  root_block_device {
    volume_type           = "gp3"
    encrypted             = true
    kms_key_id            = aws_kms_key.ebs.arn
    delete_on_termination = true
  }

  # Burstable types: never billed for surplus credits.
  dynamic "credit_specification" {
    for_each = data.aws_ec2_instance_type.dba_bastion[0].burstable_performance_supported ? [1] : []
    content {
      cpu_credits = "standard"
    }
  }

  # No key pair and no SSH daemon: Session Manager is the only way in. The agent comes
  # first, so nothing that follows can keep it from starting.
  user_data_replace_on_change = true
  user_data                   = <<-EOT
    #!/bin/bash
    set -eu
    systemctl enable --now amazon-ssm-agent.service
    systemctl disable --now sshd.service
  EOT

  tags = { Name = "${local.name}-dba-bastion", Component = "dba-access" }

  lifecycle {
    # A new AMI replaces the instance: bring the new one up before the old one goes.
    create_before_destroy = true

    precondition {
      condition     = contains(local.azs, local.dba_bastion_zone)
      error_message = "dba_bastion_availability_zone must be one of the VPC's zones: ${join(", ", local.azs)}."
    }
  }
}

# -----------------------------------------------------------------------------------------
# Instance role: Session Manager only
# -----------------------------------------------------------------------------------------

data "aws_iam_policy_document" "dba_bastion_assume" {
  statement {
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["ec2.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "dba_bastion" {
  count = var.dba_bastion_enabled ? 1 : 0

  name               = "${local.name}-dba-bastion"
  description        = "DBA bastion: SSM agent only (Session Manager port forwarding to RDS)"
  assume_role_policy = data.aws_iam_policy_document.dba_bastion_assume.json

  tags = { Name = "${local.name}-dba-bastion" }
}

resource "aws_iam_role_policy_attachment" "dba_bastion_ssm" {
  count = var.dba_bastion_enabled ? 1 : 0

  role       = aws_iam_role.dba_bastion[0].name
  policy_arn = "arn:${local.partition}:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

# Only when the account's Session Manager preferences encrypt session data with KMS: the
# agent decrypts it with this key, or every session fails.
resource "aws_iam_role_policy" "dba_bastion_session_kms" {
  count = var.dba_bastion_enabled && var.dba_session_kms_key_arn != null ? 1 : 0

  name = "decrypt-session-data"
  role = aws_iam_role.dba_bastion[0].id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Sid      = "DecryptSessionManagerData"
      Effect   = "Allow"
      Action   = ["kms:Decrypt"]
      Resource = [var.dba_session_kms_key_arn]
    }]
  })
}

resource "aws_iam_instance_profile" "dba_bastion" {
  count = var.dba_bastion_enabled ? 1 : 0

  name = "${local.name}-dba-bastion"
  role = aws_iam_role.dba_bastion[0].name

  tags = { Name = "${local.name}-dba-bastion" }
}

# -----------------------------------------------------------------------------------------
# Security groups: no inbound to the bastion; the database admits it on 5432
# -----------------------------------------------------------------------------------------

resource "aws_security_group" "dba_bastion" {
  count = var.dba_bastion_enabled ? 1 : 0

  name        = "${local.name}-dba-bastion"
  description = "DBA bastion: no inbound; out to the RDS instance (5432) and the VPC endpoints (443)"
  vpc_id      = aws_vpc.this.id

  tags = { Name = "${local.name}-dba-bastion" }
}

resource "aws_vpc_security_group_egress_rule" "dba_bastion_to_db" {
  count = var.dba_bastion_enabled ? 1 : 0

  security_group_id            = aws_security_group.dba_bastion[0].id
  description                  = "PostgreSQL to the RDS instance (Session Manager port forwarding)"
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
  referenced_security_group_id = aws_security_group.db.id

  tags = { Name = "${local.name}-dba-bastion-to-db" }
}

resource "aws_vpc_security_group_egress_rule" "dba_bastion_to_endpoints" {
  count = var.dba_bastion_enabled ? 1 : 0

  security_group_id            = aws_security_group.dba_bastion[0].id
  description                  = "HTTPS to interface VPC endpoints (ssm, ssmmessages, ec2messages, kms)"
  ip_protocol                  = "tcp"
  from_port                    = 443
  to_port                      = 443
  referenced_security_group_id = aws_security_group.endpoints.id

  tags = { Name = "${local.name}-dba-bastion-to-endpoints" }
}

resource "aws_vpc_security_group_ingress_rule" "endpoints_from_dba_bastion" {
  count = var.dba_bastion_enabled ? 1 : 0

  security_group_id            = aws_security_group.endpoints.id
  description                  = "HTTPS from the DBA bastion (SSM agent)"
  ip_protocol                  = "tcp"
  from_port                    = 443
  to_port                      = 443
  referenced_security_group_id = aws_security_group.dba_bastion[0].id

  tags = { Name = "${local.name}-endpoints-from-dba-bastion" }
}

# The database security group's only addition: 5432 from the bastion, nowhere else.
resource "aws_vpc_security_group_ingress_rule" "db_from_dba_bastion" {
  count = var.dba_bastion_enabled ? 1 : 0

  security_group_id            = aws_security_group.db.id
  description                  = "PostgreSQL from the DBA bastion (schema owner, SSM port forwarding)"
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
  referenced_security_group_id = aws_security_group.dba_bastion[0].id

  tags = { Name = "${local.name}-db-from-dba-bastion" }
}

# -----------------------------------------------------------------------------------------
# dba-tunnel: what a DBA needs, nothing more. Terraform does not attach it to anyone.
# -----------------------------------------------------------------------------------------

data "aws_iam_policy_document" "dba_tunnel" {
  count = var.dba_bastion_enabled ? 1 : 0

  statement {
    sid     = "PortForwardingThroughTheBastionOnly"
    actions = ["ssm:StartSession"]
    resources = [
      aws_instance.dba_bastion[0].arn,
      "arn:${local.partition}:ssm:${var.region}::document/${local.dba_session_document}",
    ]

    # Session Manager checks the caller's access to the session document too, including
    # the default shell document when none is named: anything but port forwarding fails.
    condition {
      test     = "BoolIfExists"
      variable = "ssm:SessionDocumentAccessCheck"
      values   = ["true"]
    }
  }

  statement {
    sid       = "EndOrResumeSessions"
    actions   = ["ssm:TerminateSession", "ssm:ResumeSession"]
    resources = ["arn:${local.partition}:ssm:${var.region}:${local.account_id}:session/*"]
  }

  statement {
    sid       = "CheckTheBastionIsOnline"
    actions   = ["ssm:DescribeInstanceInformation"]
    resources = ["*"] # no resource-level permissions for this action
  }

  statement {
    sid       = "ReadTheMasterUserSecret"
    actions   = ["secretsmanager:GetSecretValue", "secretsmanager:DescribeSecret"]
    resources = [aws_db_instance.this.master_user_secret[0].secret_arn]
  }

  statement {
    sid       = "DecryptItThroughSecretsManager"
    actions   = ["kms:Decrypt"]
    resources = [aws_kms_key.secrets.arn]

    condition {
      test     = "StringEquals"
      variable = "kms:ViaService"
      values   = ["secretsmanager.${var.region}.amazonaws.com"]
    }
  }

  dynamic "statement" {
    for_each = var.dba_session_kms_key_arn != null ? [var.dba_session_kms_key_arn] : []
    content {
      sid       = "EncryptSessionManagerData"
      actions   = ["kms:GenerateDataKey"]
      resources = [statement.value]
    }
  }
}

resource "aws_iam_policy" "dba_tunnel" {
  count = var.dba_bastion_enabled ? 1 : 0

  name        = "${local.name}-dba-tunnel"
  description = "DBA access to RDS: Session Manager port forwarding through the DBA bastion, and the master user secret. Attach it to the DBA's role or IAM Identity Center permission set."
  policy      = data.aws_iam_policy_document.dba_tunnel[0].json

  tags = { Name = "${local.name}-dba-tunnel" }
}
