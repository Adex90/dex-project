# Three tiers per availability zone:
#   public: ALB and NAT gateways (the only subnets with a route to the internet gateway)
#   app:    ECS Fargate tasks; outbound through the NAT gateway of the same zone
#   data:   RDS and RDS Proxy; local route only, no path to or from the internet
#
# Address plan (for a /16; a /17 or /18 scales down proportionally):
#   app    /19 at index 0..2 of the /19 blocks (8,190 addresses per zone for task ENIs)
#   public /22 at index 48..50 of the /22 blocks (inside the 7th /19)
#   data   /22 at index 56..58 of the /22 blocks (inside the 8th /19)
#   the 4th to 6th /19 blocks are left free for growth.

locals {
  az_index = { for i, az in local.azs : az => i }

  app_subnet_cidrs    = { for az, i in local.az_index : az => cidrsubnet(var.vpc_cidr, 3, i) }
  public_subnet_cidrs = { for az, i in local.az_index : az => cidrsubnet(var.vpc_cidr, 6, 48 + i) }
  data_subnet_cidrs   = { for az, i in local.az_index : az => cidrsubnet(var.vpc_cidr, 6, 56 + i) }

  nat_azs = var.single_nat_gateway ? [local.azs[0]] : local.azs

  # AWS APIs used by the Fargate agent (image pulls, secrets, logs) stay inside the VPC.
  interface_endpoints = ["secretsmanager", "ecr.api", "ecr.dkr", "logs", "kms", "sts", "ssm"]
}

resource "aws_vpc" "this" {
  cidr_block           = var.vpc_cidr
  enable_dns_support   = true
  enable_dns_hostnames = true # required by the private DNS of the interface endpoints

  tags = { Name = "${local.name}-vpc" }

  lifecycle {
    precondition {
      condition     = length(local.azs) == var.availability_zone_count
      error_message = "The region does not have ${var.availability_zone_count} available availability zones."
    }
  }
}

# The default security group of the VPC: no rules, so nothing can use it by accident.
resource "aws_default_security_group" "this" {
  vpc_id = aws_vpc.this.id

  tags = { Name = "${local.name}-default-deny-all" }
}

resource "aws_internet_gateway" "this" {
  vpc_id = aws_vpc.this.id

  tags = { Name = "${local.name}-igw" }
}

# -----------------------------------------------------------------------------------------
# Subnets
# -----------------------------------------------------------------------------------------

resource "aws_subnet" "public" {
  for_each = local.public_subnet_cidrs

  vpc_id                  = aws_vpc.this.id
  availability_zone       = each.key
  cidr_block              = each.value
  map_public_ip_on_launch = false # the ALB and NAT gateways get their addresses explicitly

  tags = { Name = "${local.name}-public-${each.key}", Tier = "public" }
}

resource "aws_subnet" "app" {
  for_each = local.app_subnet_cidrs

  vpc_id            = aws_vpc.this.id
  availability_zone = each.key
  cidr_block        = each.value

  tags = { Name = "${local.name}-app-${each.key}", Tier = "app" }
}

resource "aws_subnet" "data" {
  for_each = local.data_subnet_cidrs

  vpc_id            = aws_vpc.this.id
  availability_zone = each.key
  cidr_block        = each.value

  tags = { Name = "${local.name}-data-${each.key}", Tier = "data" }
}

# -----------------------------------------------------------------------------------------
# NAT gateways (outbound only: provider APIs; their addresses are the egress_public_ips output)
# -----------------------------------------------------------------------------------------

resource "aws_eip" "nat" {
  for_each = toset(local.nat_azs)

  domain = "vpc"

  tags = { Name = "${local.name}-nat-${each.key}" }

  depends_on = [aws_internet_gateway.this]
}

resource "aws_nat_gateway" "this" {
  for_each = toset(local.nat_azs)

  allocation_id     = aws_eip.nat[each.key].id
  subnet_id         = aws_subnet.public[each.key].id
  connectivity_type = "public"

  tags = { Name = "${local.name}-nat-${each.key}" }

  depends_on = [aws_internet_gateway.this]
}

# -----------------------------------------------------------------------------------------
# Route tables
# -----------------------------------------------------------------------------------------

resource "aws_route_table" "public" {
  vpc_id = aws_vpc.this.id

  tags = { Name = "${local.name}-public" }
}

resource "aws_route" "public_internet" {
  route_table_id         = aws_route_table.public.id
  destination_cidr_block = "0.0.0.0/0"
  gateway_id             = aws_internet_gateway.this.id
}

resource "aws_route_table_association" "public" {
  for_each = aws_subnet.public

  subnet_id      = each.value.id
  route_table_id = aws_route_table.public.id
}

resource "aws_route_table" "app" {
  for_each = aws_subnet.app

  vpc_id = aws_vpc.this.id

  tags = { Name = "${local.name}-app-${each.key}" }
}

resource "aws_route" "app_nat" {
  for_each = aws_route_table.app

  route_table_id         = each.value.id
  destination_cidr_block = "0.0.0.0/0"
  nat_gateway_id         = aws_nat_gateway.this[var.single_nat_gateway ? local.azs[0] : each.key].id
}

resource "aws_route_table_association" "app" {
  for_each = aws_subnet.app

  subnet_id      = each.value.id
  route_table_id = aws_route_table.app[each.key].id
}

# Data tier: only the implicit local route. No internet gateway, no NAT, no endpoints.
resource "aws_route_table" "data" {
  vpc_id = aws_vpc.this.id

  tags = { Name = "${local.name}-data-isolated" }
}

resource "aws_route_table_association" "data" {
  for_each = aws_subnet.data

  subnet_id      = each.value.id
  route_table_id = aws_route_table.data.id
}

# -----------------------------------------------------------------------------------------
# VPC endpoints
# -----------------------------------------------------------------------------------------

# S3 gateway endpoint for the app tier: ECR serves image layers from S3. The policy only
# allows reading ECR's layer bucket (presigned by ECR, so no account condition here).
resource "aws_vpc_endpoint" "s3" {
  vpc_id            = aws_vpc.this.id
  service_name      = "com.amazonaws.${var.region}.s3"
  vpc_endpoint_type = "Gateway"
  route_table_ids   = [for rt in aws_route_table.app : rt.id]

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Sid       = "EcrImageLayersOnly"
      Effect    = "Allow"
      Principal = "*"
      Action    = ["s3:GetObject"]
      Resource  = ["arn:${local.partition}:s3:::prod-${var.region}-starport-layer-bucket/*"]
    }]
  })

  tags = { Name = "${local.name}-s3" }
}

resource "aws_vpc_endpoint" "interface" {
  for_each = toset(local.interface_endpoints)

  vpc_id              = aws_vpc.this.id
  service_name        = "com.amazonaws.${var.region}.${each.key}"
  vpc_endpoint_type   = "Interface"
  subnet_ids          = [for s in aws_subnet.app : s.id]
  security_group_ids  = [aws_security_group.endpoints.id]
  private_dns_enabled = true

  # Data perimeter: the endpoints only serve principals of this account, so stolen
  # credentials of another account cannot be used to move data out through them.
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Sid       = "PrincipalsOfThisAccountOnly"
      Effect    = "Allow"
      Principal = "*"
      Action    = "*"
      Resource  = "*"
      Condition = { StringEquals = { "aws:PrincipalAccount" = local.account_id } }
    }]
  })

  tags = { Name = "${local.name}-${replace(each.key, ".", "-")}" }
}

# -----------------------------------------------------------------------------------------
# VPC flow logs (all traffic, KMS-encrypted CloudWatch log group)
# -----------------------------------------------------------------------------------------

data "aws_iam_policy_document" "flow_logs_assume" {
  statement {
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["vpc-flow-logs.amazonaws.com"]
    }

    condition {
      test     = "StringEquals"
      variable = "aws:SourceAccount"
      values   = [local.account_id]
    }

    condition {
      test     = "ArnLike"
      variable = "aws:SourceArn"
      values   = ["arn:${local.partition}:ec2:${var.region}:${local.account_id}:vpc-flow-log/*"]
    }
  }
}

resource "aws_iam_role" "flow_logs" {
  name               = "${local.name}-vpc-flow-logs"
  assume_role_policy = data.aws_iam_policy_document.flow_logs_assume.json

  tags = { Name = "${local.name}-vpc-flow-logs" }
}

data "aws_iam_policy_document" "flow_logs" {
  statement {
    actions = [
      "logs:CreateLogStream",
      "logs:PutLogEvents",
      "logs:DescribeLogGroups",
      "logs:DescribeLogStreams",
    ]
    resources = [
      aws_cloudwatch_log_group.vpc_flow.arn,
      "${aws_cloudwatch_log_group.vpc_flow.arn}:*",
    ]
  }
}

resource "aws_iam_role_policy" "flow_logs" {
  name   = "write-flow-logs"
  role   = aws_iam_role.flow_logs.id
  policy = data.aws_iam_policy_document.flow_logs.json
}

resource "aws_flow_log" "vpc" {
  vpc_id                   = aws_vpc.this.id
  traffic_type             = "ALL"
  log_destination_type     = "cloud-watch-logs"
  log_destination          = aws_cloudwatch_log_group.vpc_flow.arn
  iam_role_arn             = aws_iam_role.flow_logs.arn
  max_aggregation_interval = 60

  tags = { Name = "${local.name}-vpc-flow-logs" }
}

# -----------------------------------------------------------------------------------------
# Network ACL for the data subnets (defence in depth behind the security groups)
#
# Allowed, and nothing else:
#   app  -> data  tcp/5432           backend -> RDS Proxy, migrate task -> RDS
#   data -> data  tcp/5432           RDS Proxy -> RDS across zones
#   replies on ephemeral ports (1024-65535) in the opposite directions
# NACLs only see traffic that crosses a subnet boundary.
# -----------------------------------------------------------------------------------------

locals {
  data_nacl_ingress = concat(
    [for i, az in local.azs : { rule_no = 100 + i, cidr = local.app_subnet_cidrs[az], from = 5432, to = 5432 }],
    [for i, az in local.azs : { rule_no = 200 + i, cidr = local.data_subnet_cidrs[az], from = 5432, to = 5432 }],
    [for i, az in local.azs : { rule_no = 300 + i, cidr = local.data_subnet_cidrs[az], from = 1024, to = 65535 }],
  )
  data_nacl_egress = concat(
    [for i, az in local.azs : { rule_no = 100 + i, cidr = local.app_subnet_cidrs[az], from = 1024, to = 65535 }],
    [for i, az in local.azs : { rule_no = 200 + i, cidr = local.data_subnet_cidrs[az], from = 5432, to = 5432 }],
    [for i, az in local.azs : { rule_no = 300 + i, cidr = local.data_subnet_cidrs[az], from = 1024, to = 65535 }],
  )
}

resource "aws_network_acl" "data" {
  vpc_id     = aws_vpc.this.id
  subnet_ids = [for s in aws_subnet.data : s.id]

  dynamic "ingress" {
    for_each = local.data_nacl_ingress
    content {
      rule_no    = ingress.value.rule_no
      action     = "allow"
      protocol   = "tcp"
      cidr_block = ingress.value.cidr
      from_port  = ingress.value.from
      to_port    = ingress.value.to
    }
  }

  dynamic "egress" {
    for_each = local.data_nacl_egress
    content {
      rule_no    = egress.value.rule_no
      action     = "allow"
      protocol   = "tcp"
      cidr_block = egress.value.cidr
      from_port  = egress.value.from
      to_port    = egress.value.to
    }
  }

  tags = { Name = "${local.name}-data" }
}
