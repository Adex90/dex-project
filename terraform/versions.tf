terraform {
  # 1.10 introduced S3-native state locking (`use_lockfile`, GA in 1.11).
  required_version = ">= 1.10.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.70"
    }
    random = {
      source  = "hashicorp/random"
      version = "~> 3.7"
    }
    # Fetches the regional RDS CA bundle (see rds.tf, local.rds_ca_bundle_pem).
    http = {
      source  = "hashicorp/http"
      version = "~> 3.5"
    }
  }

  # Partial configuration: bucket, key, region, kms_key_id and use_lockfile come from
  # `terraform init -backend-config=backend.hcl` (see backend.hcl.example). The state
  # holds generated credentials (secrets.tf), so the bucket must be encrypted with a
  # CMK, versioned, private and readable only by the deployment role.
  backend "s3" {}
}
