// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "Rozu",
    platforms: [.macOS(.v13)],
    products: [.executable(name: "Rozu", targets: ["Rozu"])],
    targets: [.executableTarget(name: "Rozu")]
)
