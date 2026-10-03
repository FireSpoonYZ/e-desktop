// Orca: src/main/daemon/startup-device-attributes-responder.ts
// de8bffe24045b396212f4f63de8960ec8380ea07 | MIT Copyright (c) 2026 Lovecast Inc.
// Adaptation: copied responder only; output-stripping filter intentionally omitted.
import type { Terminal } from '@xterm/headless'
type DeviceAttributesParser = Pick<Terminal['parser'], 'registerCsiHandler'>

/** Returns a disposer so a caller scoped to startup can hand DA1 back to the
 *  renderer once its window closes. */
export function installDeviceAttributesResponder(deps: {
  parser: DeviceAttributesParser
  response: string
  reply: (data: string) => void
}): () => void {
  const handler = deps.parser.registerCsiHandler({ final: 'c' }, (params) => {
    // Why the param check: only DA1 is answered here. Secondary/tertiary variants
    // carry a prefix and must fall through to the renderer.
    const isPrimaryQuery = params.length === 0 || (params.length === 1 && params[0] === 0)
    if (!isPrimaryQuery) {
      return false
    }
    deps.reply(deps.response)
    return true
  })
  return () => handler.dispose()
}
