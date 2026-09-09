import AppKit

guard CommandLine.arguments.count == 2 else { fatalError("Pass an output .iconset directory") }
let output = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)

for (name, size) in [
    ("icon_16x16", 16), ("icon_16x16@2x", 32),
    ("icon_32x32", 32), ("icon_32x32@2x", 64),
    ("icon_128x128", 128), ("icon_128x128@2x", 256),
    ("icon_256x256", 256), ("icon_256x256@2x", 512),
    ("icon_512x512", 512), ("icon_512x512@2x", 1024)
] {
    let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size,
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)
    let scale = CGFloat(size) / 40
    let transform = NSAffineTransform()
    transform.scale(by: scale)
    transform.concat()
    NSColor(calibratedWhite: 180.0 / 255, alpha: 1).setFill()
    NSBezierPath(roundedRect: NSRect(x: 2, y: 2, width: 36, height: 36), xRadius: 9, yRadius: 9).fill()
    let r = NSBezierPath()
    r.move(to: NSPoint(x: 14, y: 11))
    r.line(to: NSPoint(x: 14, y: 27))
    r.line(to: NSPoint(x: 19, y: 27))
    r.line(to: NSPoint(x: 19, y: 24))
    r.curve(to: NSPoint(x: 26, y: 27), controlPoint1: NSPoint(x: 20, y: 26), controlPoint2: NSPoint(x: 22, y: 27))
    r.line(to: NSPoint(x: 26, y: 22))
    r.curve(to: NSPoint(x: 19, y: 17), controlPoint1: NSPoint(x: 21, y: 23), controlPoint2: NSPoint(x: 19, y: 20))
    r.line(to: NSPoint(x: 19, y: 11))
    r.close()
    NSColor(calibratedWhite: 13.0 / 255, alpha: 1).setFill()
    r.fill()
    NSGraphicsContext.restoreGraphicsState()
    try bitmap.representation(using: .png, properties: [:])!.write(to: output.appendingPathComponent(name + ".png"))
}
