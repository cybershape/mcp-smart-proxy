class Msp < Formula
  desc "Smart proxy for multiple stdio MCP servers"
  homepage "https://github.com/tiejunhu/mcp-smart-proxy"
  version "0.0.3"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/tiejunhu/mcp-smart-proxy/releases/download/v0.0.3/msp-v0.0.3-aarch64-apple-darwin.tar.gz"
      sha256 "bff5e1ae1e24bbdd6b86bbd707f5c88c10a0ba40dbe8278ab1c566091c70e474"
    else
      url "https://github.com/tiejunhu/mcp-smart-proxy/releases/download/v0.0.3/msp-v0.0.3-x86_64-apple-darwin.tar.gz"
      sha256 "0189a2f5c95f675c142de1dd6722a5df767aaf61ed61f172b72990238a62eed6"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/tiejunhu/mcp-smart-proxy/releases/download/v0.0.3/msp-v0.0.3-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "89a221edcdf9e621d6151a9e344779fd99daf7ef8fab1f105312dce353d49969"
    else
      url "https://github.com/tiejunhu/mcp-smart-proxy/releases/download/v0.0.3/msp-v0.0.3-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "2b9f1006f324966bcfff29993d01ca9ed8c6e9b8eaba99b75cfc3fd30c245931"
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
