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
    // Bloom vector, matching assets/brand/bloom/bloom-graphite.svg.
    let bloomTransform = NSAffineTransform()
    bloomTransform.translateX(by: 5, yBy: 35)
    bloomTransform.scaleX(by: 30.0 / 365, yBy: -30.0 / 365)
    bloomTransform.translateX(by: -1097, yBy: -221)
    bloomTransform.concat()
    NSColor(calibratedWhite: 13.0 / 255, alpha: 1).setFill()
    let petal0 = NSBezierPath()
    petal0.move(to: NSPoint(x: 1230, y: 246))
    petal0.curve(to: NSPoint(x: 1380, y: 327), controlPoint1: NSPoint(x: 1301, y: 228), controlPoint2: NSPoint(x: 1374, y: 272))
    petal0.curve(to: NSPoint(x: 1346, y: 425), controlPoint1: NSPoint(x: 1384, y: 364), controlPoint2: NSPoint(x: 1369, y: 404))
    petal0.curve(to: NSPoint(x: 1329, y: 417), controlPoint1: NSPoint(x: 1337, y: 433), controlPoint2: NSPoint(x: 1328, y: 427))
    petal0.curve(to: NSPoint(x: 1294, y: 353), controlPoint1: NSPoint(x: 1331, y: 393), controlPoint2: NSPoint(x: 1318, y: 366))
    petal0.curve(to: NSPoint(x: 1203, y: 337), controlPoint1: NSPoint(x: 1270, y: 340), controlPoint2: NSPoint(x: 1239, y: 339))
    petal0.curve(to: NSPoint(x: 1198, y: 301), controlPoint1: NSPoint(x: 1184, y: 337), controlPoint2: NSPoint(x: 1189, y: 319))
    petal0.curve(to: NSPoint(x: 1230, y: 246), controlPoint1: NSPoint(x: 1206, y: 280), controlPoint2: NSPoint(x: 1214, y: 260))
    petal0.close()
    petal0.fill()
    let petal1 = NSBezierPath()
    petal1.move(to: NSPoint(x: 1215, y: 359))
    petal1.curve(to: NSPoint(x: 1300, y: 367), controlPoint1: NSPoint(x: 1245, y: 348), controlPoint2: NSPoint(x: 1280, y: 349))
    petal1.curve(to: NSPoint(x: 1279, y: 396), controlPoint1: NSPoint(x: 1310, y: 377), controlPoint2: NSPoint(x: 1295, y: 385))
    petal1.curve(to: NSPoint(x: 1267, y: 510), controlPoint1: NSPoint(x: 1240, y: 426), controlPoint2: NSPoint(x: 1246, y: 475))
    petal1.curve(to: NSPoint(x: 1260, y: 538), controlPoint1: NSPoint(x: 1273, y: 523), controlPoint2: NSPoint(x: 1277, y: 536))
    petal1.curve(to: NSPoint(x: 1117, y: 459), controlPoint1: NSPoint(x: 1191, y: 543), controlPoint2: NSPoint(x: 1120, y: 512))
    petal1.curve(to: NSPoint(x: 1215, y: 359), controlPoint1: NSPoint(x: 1114, y: 415), controlPoint2: NSPoint(x: 1152, y: 374))
    petal1.close()
    petal1.fill()
    let petal2 = NSBezierPath()
    petal2.move(to: NSPoint(x: 1390, y: 380))
    petal2.curve(to: NSPoint(x: 1435, y: 485), controlPoint1: NSPoint(x: 1425, y: 404), controlPoint2: NSPoint(x: 1446, y: 446))
    petal2.curve(to: NSPoint(x: 1337, y: 548), controlPoint1: NSPoint(x: 1423, y: 534), controlPoint2: NSPoint(x: 1381, y: 562))
    petal2.curve(to: NSPoint(x: 1260, y: 464), controlPoint1: NSPoint(x: 1295, y: 534), controlPoint2: NSPoint(x: 1266, y: 493))
    petal2.curve(to: NSPoint(x: 1270, y: 441), controlPoint1: NSPoint(x: 1255, y: 447), controlPoint2: NSPoint(x: 1261, y: 438))
    petal2.curve(to: NSPoint(x: 1350, y: 428), controlPoint1: NSPoint(x: 1308, y: 461), controlPoint2: NSPoint(x: 1329, y: 453))
    petal2.curve(to: NSPoint(x: 1390, y: 380), controlPoint1: NSPoint(x: 1367, y: 410), controlPoint2: NSPoint(x: 1376, y: 376))
    petal2.close()
    petal2.fill()
    NSGraphicsContext.restoreGraphicsState()
    try bitmap.representation(using: .png, properties: [:])!.write(to: output.appendingPathComponent(name + ".png"))
}
