output "automator_instance_ids" { value = module.automators.instance_ids }
output "metadata_bucket" { value = module.metadata.bucket_name }
output "db_endpoint" { value = module.database.address }
output "db_password_parameter" { value = module.database.password_parameter_name }
output "ssm_session_hint" {
  value = "aws ssm start-session --target <instance-id>   # no SSH, no inbound ports"
}
