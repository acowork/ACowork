/**
 * Boot gate screens (v1.1 §7). The App renders one of these instead of the
 * tab shell until `authStore.phase === 'ready'`.
 *
 * They are dumb: every decision (probe, login, refresh, expiry) lives in
 * `authStore` / `lib/api`, because Mobile — like Desktop — carries no
 * business logic in the view layer.
 */

import { useState } from 'react'
import { useAuthStore } from '../../stores/authStore'

function Shell({ children }: { children: React.ReactNode }) {
  return <div className="app"><div className="boot-screen">{children}</div></div>
}

export function ConnectScreen() {
  const probe = useAuthStore((s) => s.probe)
  const busy = useAuthStore((s) => s.busy)
  const [url, setUrl] = useState('')
  const [err, setErr] = useState('')

  const submit = async () => {
    setErr('')
    if (!/^https?:\/\/.+/.test(url.trim())) {
      setErr('请输入完整地址，例如 http://192.168.1.23:19876')
      return
    }
    const ok = await probe(url)
    if (!ok) setErr('无法连接：请确认桌面端已开启「允许局域网连接」，且两台设备在同一 Wi-Fi')
  }

  return (
    <Shell>
      <div className="boot-logo">A</div>
      <div className="boot-title">连接 ACowork</div>
      <div className="boot-sub">输入桌面端共享的网关地址。移动端不运行本地网关。</div>
      <div className="boot-form">
        <input
          className={`boot-input${err ? ' is-error' : ''}`}
          placeholder="http://192.168.1.23:19876"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && void submit()}
          autoComplete="off"
          spellCheck={false}
        />
        {err ? <div className="boot-error">{err}</div> : null}
        <button type="button" className="boot-primary" onClick={() => void submit()} disabled={busy}>
          {busy ? '检测中…' : '连接'}
        </button>
        <div className="boot-hint">v1 不支持扫码配对与注册：没有账号请在桌面端创建。</div>
      </div>
    </Shell>
  )
}

export function LoginScreen() {
  const login = useAuthStore((s) => s.login)
  const busy = useAuthStore((s) => s.busy)
  const loginError = useAuthStore((s) => s.loginError)
  const baseUrl = useAuthStore((s) => s.baseUrl)
  const changeAddress = useAuthStore((s) => s.changeAddress)
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')

  const submit = () => {
    if (!username || !password) return
    void login(username, password)
  }

  return (
    <Shell>
      <div className="boot-title">登录</div>
      <div className="boot-sub">{baseUrl}</div>
      <div className="boot-form">
        <input
          className="boot-input"
          placeholder="用户名"
          value={username}
          onChange={(e) => setUsername(e.target.value)}
          autoComplete="username"
        />
        <input
          className={`boot-input${loginError ? ' is-error' : ''}`}
          type="password"
          placeholder="密码"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && submit()}
          autoComplete="current-password"
        />
        {loginError ? <div className="boot-error">{loginError}</div> : null}
        <button type="button" className="boot-primary" onClick={submit} disabled={busy || !username || !password}>
          {busy ? '登录中…' : '登录'}
        </button>
        <button type="button" className="boot-secondary" onClick={changeAddress}>
          更换网关地址
        </button>
        <div className="boot-hint">账号由桌面端管理；忘记密码请在桌面端重置。</div>
      </div>
    </Shell>
  )
}

export function DisconnectedScreen() {
  const baseUrl = useAuthStore((s) => s.baseUrl)
  const retry = useAuthStore((s) => s.retryConnection)
  const change = useAuthStore((s) => s.changeAddress)
  const busy = useAuthStore((s) => s.busy)

  return (
    <Shell>
      <div className="boot-title">连接已断开</div>
      <div className="boot-sub">
        无法访问 {baseUrl || '网关'}。
        请检查桌面端是否在线、两台设备是否在同一网络。
      </div>
      <div className="boot-form">
        <button type="button" className="boot-primary" onClick={() => void retry()} disabled={busy}>
          {busy ? '重试中…' : '重试'}
        </button>
        <button type="button" className="boot-secondary" onClick={change}>
          更换地址
        </button>
      </div>
    </Shell>
  )
}

export function RestrictedScreen() {
  const retry = useAuthStore((s) => s.retryConnection)
  const busy = useAuthStore((s) => s.busy)
  return (
    <Shell>
      <div className="boot-title">受限模式</div>
      <div className="boot-sub">网关尚未完成初始化设置（或注册未开放）。请在桌面端完成设置后再使用移动端。</div>
      <div className="boot-form">
        <button type="button" className="boot-primary" onClick={() => void retry()} disabled={busy}>
          {busy ? '检测中…' : '重新检测'}
        </button>
      </div>
    </Shell>
  )
}

export function BootingScreen() {
  return (
    <Shell>
      <div className="boot-logo">A</div>
      <div className="boot-sub">加载中…</div>
    </Shell>
  )
}
