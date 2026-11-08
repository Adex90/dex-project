# Automator / Searcher hosts. Free Tier = 750 instance-hours per month in TOTAL (one t2/t3.micro 24x7 uses it
# all), so the default is a single instance. Admin access is SSM Session Manager only: no SSH key, no port 22.
data "aws_region" "current" {}
data "aws_caller_identity" "current" {}

# Latest Amazon Linux 2023 x86_64 (resolved through the public SSM parameter, not a hard-coded AMI id)
data "aws_ssm_parameter" "al2023" {
  name = "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64"
}

data "aws_iam_policy_document" "assume" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["ec2.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "bot" {
  name_prefix        = "${var.name}-bot-"
  assume_role_policy = data.aws_iam_policy_document.assume.json
}

resource "aws_iam_role_policy_attachment" "ssm_core" {
  role       = aws_iam_role.bot.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

# Least privilege: read ONLY this project's parameters, read/write ONLY the metadata bucket.
# Note: the bots hold NO signing key. Transactions are signed by a remote MPC/TSS signer.
data "aws_iam_policy_document" "bot" {
  statement {
    sid       = "ReadOwnParameters"
    actions   = ["ssm:GetParameter", "ssm:GetParameters", "ssm:GetParametersByPath"]
    resources = ["arn:aws:ssm:${data.aws_region.current.name}:${data.aws_caller_identity.current.account_id}:parameter/${var.name}/*"]
  }
  statement {
    sid       = "ListMetadataBucket"
    actions   = ["s3:ListBucket"]
    resources = [var.metadata_bucket_arn]
  }
  statement {
    sid       = "ReadWriteMetadataObjects"
    actions   = ["s3:GetObject", "s3:PutObject"]
    resources = ["${var.metadata_bucket_arn}/automator/*"]
  }
  # The bucket is SSE-KMS with a customer managed key: S3 needs these on the caller's behalf, and only S3 may use them.
  statement {
    sid       = "UseMetadataKeyThroughS3Only"
    actions   = ["kms:GenerateDataKey", "kms:Decrypt"]
    resources = [var.metadata_kms_key_arn]
    condition {
      test     = "StringEquals"
      variable = "kms:ViaService"
      values   = ["s3.${data.aws_region.current.name}.amazonaws.com"]
    }
  }
}

resource "aws_iam_role_policy" "bot" {
  name   = "${var.name}-bot"
  role   = aws_iam_role.bot.id
  policy = data.aws_iam_policy_document.bot.json
}

resource "aws_iam_instance_profile" "bot" {
  name_prefix = "${var.name}-bot-"
  role        = aws_iam_role.bot.name
}

resource "aws_instance" "bot" {
  count = var.instance_count

  ami                         = data.aws_ssm_parameter.al2023.value
  instance_type               = var.instance_type
  subnet_id                   = var.public_subnet_ids[count.index % length(var.public_subnet_ids)]
  vpc_security_group_ids      = [var.security_group_id]
  iam_instance_profile        = aws_iam_instance_profile.bot.name
  associate_public_ip_address = true # needs outbound internet without a NAT; ingress is still fully closed

  user_data_replace_on_change = true
  user_data = templatefile("${path.module}/templates/bootstrap.sh.tftpl", {
    name        = var.name
    region      = data.aws_region.current.name
    db_host     = var.db_host
    db_port     = var.db_port
    db_name     = var.db_name
    db_user     = var.db_user
    db_password = var.db_password_parameter_name
    bucket      = var.metadata_bucket_name
  })

  metadata_options {
    http_tokens                 = "required" # IMDSv2 only (blocks SSRF-style credential theft)
    http_put_response_hop_limit = 1
    http_endpoint               = "enabled"
  }

  root_block_device {
    volume_type           = "gp3"
    volume_size           = 8 # Free Tier: 30 GB EBS total
    encrypted             = true
    delete_on_termination = true
  }

  monitoring = false # detailed monitoring is billed; basic 5-minute metrics are free

  tags = { Name = "${var.name}-bot-${count.index}", Role = "automator" }

  lifecycle {
    precondition {
      condition     = var.instance_count * 730 <= 750 || var.allow_over_free_tier
      error_message = "More than one always-on instance exceeds the 750 h/month Free Tier. Set allow_over_free_tier = true to accept the cost."
    }
  }
}
