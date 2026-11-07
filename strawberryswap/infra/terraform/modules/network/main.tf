# Network for the Automator tier.
#   public subnets  : EC2 bots (route to an Internet Gateway; they must reach RPC nodes and data providers)
#   private subnets : RDS only. NO route to the internet and NO NAT gateway (a NAT costs ~$32/month and is
#                     not Free Tier). The database therefore has no path out or in except from the bot SG.
data "aws_availability_zones" "available" {
  state = "available"
}

locals {
  azs = slice(data.aws_availability_zones.available.names, 0, 2) # RDS subnet groups need >= 2 AZs
}

resource "aws_vpc" "this" {
  cidr_block           = var.vpc_cidr
  enable_dns_support   = true
  enable_dns_hostnames = true
  tags                 = { Name = "${var.name}-vpc" }
}

resource "aws_internet_gateway" "this" {
  vpc_id = aws_vpc.this.id
  tags   = { Name = "${var.name}-igw" }
}

resource "aws_subnet" "public" {
  count                   = 2
  vpc_id                  = aws_vpc.this.id
  availability_zone       = local.azs[count.index]
  cidr_block              = cidrsubnet(var.vpc_cidr, 8, count.index)
  map_public_ip_on_launch = false # instances opt in explicitly
  tags                    = { Name = "${var.name}-public-${local.azs[count.index]}", Tier = "public" }
}

resource "aws_subnet" "private" {
  count             = 2
  vpc_id            = aws_vpc.this.id
  availability_zone = local.azs[count.index]
  cidr_block        = cidrsubnet(var.vpc_cidr, 8, count.index + 10)
  tags              = { Name = "${var.name}-private-${local.azs[count.index]}", Tier = "private" }
}

resource "aws_route_table" "public" {
  vpc_id = aws_vpc.this.id
  route {
    cidr_block = "0.0.0.0/0"
    gateway_id = aws_internet_gateway.this.id
  }
  tags = { Name = "${var.name}-public-rt" }
}

resource "aws_route_table_association" "public" {
  count          = 2
  subnet_id      = aws_subnet.public[count.index].id
  route_table_id = aws_route_table.public.id
}

# Private route table intentionally has only the implicit local route.
resource "aws_route_table" "private" {
  vpc_id = aws_vpc.this.id
  tags   = { Name = "${var.name}-private-rt" }
}

resource "aws_route_table_association" "private" {
  count          = 2
  subnet_id      = aws_subnet.private[count.index].id
  route_table_id = aws_route_table.private.id
}

# Lock down the default SG (Terraform best practice: nothing should ever use it).
resource "aws_default_security_group" "default" {
  vpc_id = aws_vpc.this.id
}

# Security group for the Automator / Searcher instances.
#  * NO inbound rules at all: no SSH, no HTTP. Administration is through SSM Session Manager (outbound 443).
#  * Egress is allow-listed: HTTPS (RPC nodes, data providers, AWS APIs), Postgres inside the VPC, DNS, NTP.
resource "aws_security_group" "bots" {
  name_prefix = "${var.name}-bots-"
  description = "Automator instances: no ingress, restricted egress"
  vpc_id      = aws_vpc.this.id
  lifecycle { create_before_destroy = true }
}

resource "aws_vpc_security_group_egress_rule" "https" {
  security_group_id = aws_security_group.bots.id
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
  cidr_ipv4         = "0.0.0.0/0"
  description       = "HTTPS"
}

resource "aws_vpc_security_group_egress_rule" "postgres" {
  security_group_id = aws_security_group.bots.id
  ip_protocol       = "tcp"
  from_port         = 5432
  to_port           = 5432
  cidr_ipv4         = var.vpc_cidr
  description       = "Postgres inside the VPC"
}

resource "aws_vpc_security_group_egress_rule" "dns_udp" {
  security_group_id = aws_security_group.bots.id
  ip_protocol       = "udp"
  from_port         = 53
  to_port           = 53
  cidr_ipv4         = var.vpc_cidr
  description       = "VPC DNS resolver"
}

resource "aws_vpc_security_group_egress_rule" "dns_tcp" {
  security_group_id = aws_security_group.bots.id
  ip_protocol       = "tcp"
  from_port         = 53
  to_port           = 53
  cidr_ipv4         = var.vpc_cidr
  description       = "VPC DNS resolver (TCP fallback)"
}

resource "aws_vpc_security_group_egress_rule" "ntp" {
  security_group_id = aws_security_group.bots.id
  ip_protocol       = "udp"
  from_port         = 123
  to_port           = 123
  cidr_ipv4         = "169.254.169.123/32"
  description       = "Amazon Time Sync"
}
