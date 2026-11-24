terraform {
  # tests/ use override_during (Terraform 1.9+); CI runs 1.16.
  required_version = ">= 1.9.0"
  required_providers {
    aws = { source = "hashicorp/aws", version = ">= 5.80" }
  }
}
