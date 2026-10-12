---
name: run-app
description: Serve NEXRAD Workbench with trunk and screenshot it in headless Chromium to confirm a change works in the real app.
---

# Run and screenshot the app

The SessionStart hook (`.claude/hooks/session-start.sh`) installs trunk and
warms the build in cloud sessions. Locally, run `scripts/bootstrap.sh` and
`cargo install --locked trunk` first.

1. Serve in the background (port 8080, see `Trunk.toml`):

   ```bash
   nohup trunk serve > /tmp/trunk.log 2>&1 &
   until curl -sf -o /dev/null http://127.0.0.1:8080/; do sleep 2; done
   ```

2. Screenshot with Playwright. Headless Chromium has no GPU, so force
   SwiftShader or WebGL2 fails:

   ```js
   // node shot.cjs out.png   (NODE_PATH="$(npm root -g)" if playwright is global)
   const { chromium } = require('playwright');
   (async () => {
     const b = await chromium.launch({ args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'] });
     const p = await b.newPage({ viewport: { width: 1600, height: 1000 } });
     p.on('console', m => console.log(m.type() + ': ' + m.text()));
     p.on('pageerror', e => console.log('PAGEERROR: ' + e.message));
     await p.goto('http://127.0.0.1:8080/');
     await p.waitForTimeout(15000); // archive listing + first scan download/decode
     await p.screenshot({ path: process.argv[2] });
     await b.close();
   })();
   ```

3. Check the console log: `Archive listing received` and `Downloaded:` in the
   top bar mean S3 data loaded. A first launch shows the "Welcome" site picker
   over the map; that is expected.

Notes:
- Data comes from the public `unidata-nexrad-level2` S3 bucket; NWS alerts
  come from `api.weather.gov`. If the network allowlist blocks a host, those
  requests log `Failed to fetch` but the app still renders.
- Each run gets a fresh IndexedDB, so the cache starts empty.
