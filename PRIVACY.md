# Privacy Policy

Orbit Tape does not include telemetry, analytics, advertising, or user accounts, and does not transfer information to other networked systems unless needed to fetch data from data providers or requested by the user.

Orbit Tape connects to the third-party data providers enabled in its provider list (all supported providers are enabled by default and can be disabled). These providers receive the requested data type and identifiers, along with the user's IP address and request metadata. Requests go to a supported provider or to a data server configured by the user, which also receives the optional bearer token. Connections to user-configured data servers accept invalid TLS certificates, including self-signed ones, so they should only be used on trusted networks.

The optional indicator catalog browser contacts only its catalog address, and only when the user opens or refreshes it or installs an indicator; nothing is fetched at startup. The default address is the community catalog at `https://raw.githubusercontent.com/atraxsrc/orbit-tape-indicators/main/catalog.json`, served by GitHub; Settings > Indicator catalog (or `ORBIT_TAPE_CATALOG_URL`) sets another HTTPS address. Requests carry only the User-Agent `orbit-tape/<version>`. The catalog host may log request metadata, including IP addresses, under its own privacy policy. Not opening the browser, or installing content manually, avoids this entirely.

Indicator alerts are shown through the desktop's local notification service and are not sent anywhere else. Settings, caches, and logs are stored locally and are not encrypted or uploaded. Proxy credentials and data-server tokens are stored in the system keychain. If a proxy is configured, all application traffic uses it.

Orbit Tape does not control the privacy practices of data providers, data servers, proxies, or the indicator catalog host.
