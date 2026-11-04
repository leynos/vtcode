class Diapason < Formula
  desc "Diapason source build, a df12 Productions hard fork focused on ACP and observability"
  homepage "https://github.com/leynos/vtcode"
  head "https://github.com/leynos/vtcode.git", branch: "main"
  license "MIT OR Apache-2.0"

  depends_on "rust" => :build

  def install
    system "cargo", "build", "--release", "--locked", "--bin", "diapason"
    bin.install "target/release/diapason"
  end

  def caveats
    <<~EOS
      Diapason has no published binary release yet. This formula builds from
      the source repository. The original VTCode author, Vinh Nguyen, retains
      attribution in the source and its licence notices.

      To get started, set your provider API key and run:
        diapason --help
    EOS
  end

  test do
    assert_match "diapason", shell_output("#{bin}/diapason --version")
  end
end
