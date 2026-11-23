output "automator_instance_ids" { value = module.automators.instance_ids }
output "metadata_bucket" { value = module.metadata.bucket_name }
output "db_endpoint" { value = module.database.address }
output "db_password_parameter" { value = module.database.password_parameter_name }
output "ssm_session_hint" {
  value = "aws ssm start-session --target <instance-id>   # no SSH, no inbound ports"
}
output "signing_key_arns" {
  description = "Signer => KMS key ARN. Each signer's address: AWS_KMS_KEY_ID=<arn> cast wallet address --aws (GetPublicKey only)."
  value       = module.signing_keys.key_arns
}
output "signing_key_aliases" { value = module.signing_keys.key_aliases }
output "signer_role_arns" { value = module.signing_keys.role_arns }
