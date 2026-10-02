import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { App } from './App'
import { setChatTransport } from './stores/chatStore'
import { httpChatTransport, setGatewayConfig } from './lib/api'
import './styles/tokens.css'
import './styles/app.css'

// Wire the transport before the first render: a screen that mounts and
// immediately calls openSession would otherwise find a null transport and
// silently no-op.
setChatTransport(httpChatTransport)
setGatewayConfig({ baseUrl: import.meta.env.VITE_GATEWAY_URL ?? '' })

const el = document.getElementById('root')
if (!el) throw new Error('#root not found')
createRoot(el).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
