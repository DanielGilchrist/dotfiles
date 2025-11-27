function aws_sso_auth
  aws sso login --profile $argv && eval $(aws configure export-credentials --profile $argv --format env)
end
