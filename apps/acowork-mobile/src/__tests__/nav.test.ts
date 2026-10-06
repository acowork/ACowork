import { describe, it, expect, beforeEach } from 'vitest'
import { useNavStore, TABS } from '../stores/navStore'

beforeEach(() => {
  useNavStore.setState({
    activeTab: 'chat',
    stacks: {
      chat: ['chat/list'],
      projects: ['projects/list'],
      docs: ['docs/list'],
      settings: ['settings/root'],
    },
    direction: 'none',
  })
})

describe('navStore — four tabs', () => {
  it('exposes exactly the four approved tabs, in order', () => {
    expect(TABS.map((t) => t.key)).toEqual(['chat', 'projects', 'docs', 'settings'])
  })

  it('gives every tab its own root route', () => {
    const { stacks } = useNavStore.getState()
    expect(stacks.chat[0]).toBe('chat/list')
    expect(stacks.projects[0]).toBe('projects/list')
    expect(stacks.docs[0]).toBe('docs/list')
    expect(stacks.settings[0]).toBe('settings/root')
  })
})

describe('navStore — per-tab independent stacks', () => {
  it('keeps a deep stack when switching away and back', () => {
    const { push, switchTab } = useNavStore.getState()
    // push() targets the ACTIVE tab — you can only drill into what you are
    // looking at — so build the settings stack from the settings tab.
    switchTab('settings')
    push('settings/general')
    push('settings/gateway')
    switchTab('docs')
    switchTab('settings')

    const s = useNavStore.getState()
    expect(s.activeTab).toBe('settings')
    // Landing on the Gateway page, not the settings root — the whole reason
    // per-tab stacks exist.
    expect(s.stacks.settings).toEqual(['settings/root', 'settings/general', 'settings/gateway'])
  })

  it('does not disturb other tabs stacks when pushing', () => {
    const { push, switchTab } = useNavStore.getState()
    push('chat/detail')
    switchTab('projects')
    push('projects/detail')

    const s = useNavStore.getState()
    expect(s.stacks.chat).toEqual(['chat/list', 'chat/detail'])
    expect(s.stacks.projects).toEqual(['projects/list', 'projects/detail'])
  })
})

describe('navStore — pop semantics', () => {
  it('refuses to pop past the root', () => {
    const { pop, depth } = useNavStore.getState()
    expect(depth()).toBe(0)
    expect(pop()).toBe(false)
    expect(useNavStore.getState().stacks.chat).toEqual(['chat/list'])
  })

  it('pops one level at a time', () => {
    const { push, pop, current } = useNavStore.getState()
    push('settings/general')
    push('settings/gateway')
    expect(current()).toBe('settings/gateway')
    pop()
    expect(current()).toBe('settings/general')
  })

  it('re-tapping the active tab pops to root', () => {
    const { push, switchTab, current } = useNavStore.getState()
    push('chat/detail')
    switchTab('chat')
    expect(current()).toBe('chat/list')
  })

  it('pushing onto an untouched tab starts from its root', () => {
    const { switchTab, push, current } = useNavStore.getState()
    switchTab('docs')
    push('docs/doc')
    expect(current()).toBe('docs/doc')
  })
})
