return {
  server = {
    mason = true,
    cmd = {
      "arduino-language-server",
      "-cli-config",
      os.getenv("HOME") .. "/Library/Arduino15/arduino-cli.yaml",
      "-cli",
      "arduino-cli",
      "-clangd",
      "clangd",
    },
  },
}
