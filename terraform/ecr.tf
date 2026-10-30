# One repository per image. Tags are immutable (a tag always names the same image, so a
# task definition revision is reproducible), images are scanned on push and encrypted
# with the ECR CMK.

resource "aws_ecr_repository" "this" {
  for_each = toset(["backend", "frontend"])

  name                 = "${local.name}/${each.key}"
  image_tag_mutability = "IMMUTABLE"
  force_delete         = false

  image_scanning_configuration {
    scan_on_push = true
  }

  encryption_configuration {
    encryption_type = "KMS"
    kms_key         = aws_kms_key.ecr.arn
  }

  tags = { Name = "${local.name}/${each.key}", Component = each.key }
}

resource "aws_ecr_lifecycle_policy" "this" {
  for_each = aws_ecr_repository.this

  repository = each.value.name
  policy = jsonencode({
    rules = [
      {
        rulePriority = 1
        description  = "Expire untagged images (failed or superseded pushes) after 14 days"
        selection = {
          tagStatus   = "untagged"
          countType   = "sinceImagePushed"
          countUnit   = "days"
          countNumber = 14
        }
        action = { type = "expire" }
      },
      {
        rulePriority = 2
        description  = "Keep the 100 most recent images (rollback and forensics)"
        selection = {
          tagStatus   = "any"
          countType   = "imageCountMoreThan"
          countNumber = 100
        }
        action = { type = "expire" }
      },
    ]
  })
}
