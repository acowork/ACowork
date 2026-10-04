$r = Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | Sort-Object RouteMetric, InterfaceMetric | Select-Object -First 1
if ($r) { (Get-NetIPAddress -AddressFamily IPv4 -InterfaceIndex $r.InterfaceIndex | Where-Object IPAddress -ne '127.0.0.1' | Select-Object -First 1).IPAddress }
