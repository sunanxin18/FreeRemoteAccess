// 原生 KeyCode -> USB HID；不识别的键保持本地，不发协议猜测。
export function usbHid(key: number): number {
  if (key >= 2017 && key <= 2042) { return key - 2017 + 4; }
  if (key >= 2001 && key <= 2009) { return key - 2001 + 30; }
  if (key === 2000) { return 39; }
  if (key >= 2090 && key <= 2101) { return key - 2090 + 58; }
  switch (key) {
    case 2054: return 40; case 2070: return 41; case 2055: return 42;
    case 2049: return 43; case 2050: return 44; case 2056: return 53;
    case 2057: return 45; case 2058: return 46; case 2059: return 47;
    case 2060: return 48; case 2062: return 51; case 2063: return 52;
    case 2061: return 49; case 2043: return 54; case 2044: return 55;
    case 2064: return 56; case 2074: return 57; case 2071: return 76; case 2068: return 75; case 2069: return 78;
    case 2081: return 74; case 2082: return 77; case 2083: return 73;
    case 2079: return 70; case 2080: return 72; case 2102: return 83;
    case 2015: return 79; case 2014: return 80; case 2013: return 81; case 2012: return 82;
    case 2072: return 224; case 2047: return 225; case 2045: return 226;
    case 2076: return 227; case 2073: return 228; case 2048: return 229;
    case 2046: return 230; case 2077: return 231; default: return 0;
  }
}
