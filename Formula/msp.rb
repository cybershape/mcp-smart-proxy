class Msp < Formula
  desc "Smart proxy for multiple stdio MCP servers"
  homepage "https://github.com/tiejunhu/mcp-smart-proxy"
  version "0.0.5"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/tiejunhu/mcp-smart-proxy/releases/download/v0.0.5/msp-v0.0.5-aarch64-apple-darwin.tar.gz"
      sha256 "a333c7fabecd60715bc373e0f70190966c3a676e1c0cc77ceefd70daf5442646"
    else
      url "https://github.com/tiejunhu/mcp-smart-proxy/releases/download/v0.0.5/msp-v0.0.5-x86_64-apple-darwin.tar.gz"
      sha256 "2f25739290a2dca2bb5c42ed6f955cc8f6747a0970e7a0b93fc122dd2dc94219"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/tiejunhu/mcp-smart-proxy/releases/download/v0.0.5/msp-v0.0.5-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "f3ae7bfdf7e1f504f1ad6f31641fa603d32e31513d3f5468793f18d57f5ae991"
    else
      url "https://github.com/tiejunhu/mcp-smart-proxy/releases/download/v0.0.5/msp-v0.0.5-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "7333f30d996c68ed7fd4c6e1ffcd0146a42e06331bf498b38610f4c0de24d3a0"
    end
  end

  def install
    binary = Dir["msp", "*/msp", "mcp-smart-proxy", "*/mcp-smart-proxy"].first
    raise "msp binary not found in archive" unless binary

    readme = Dir["README.md", "*/README.md"].first
    raise "README.md not found in archive" unless readme

    bin.install binary => "msp"
    prefix.install_metafiles readme
  end

  test do
    assert_match "A smart MCP proxy", shell_output("#{bin}/msp --help")
  end
end
