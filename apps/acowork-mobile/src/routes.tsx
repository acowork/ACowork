/**
 * Route registry. Every screen the shell can render is registered here, so
 * the navigation contract is a single inspectable table rather than a set
 * of conditional imports scattered through components.
 */

import type { ComponentType } from 'react'
import { ChatListScreen } from './screens/chat/ChatListScreen'
import { ChatDetailScreen } from './screens/chat/ChatDetailScreen'
import { NotImplemented } from './components/NotImplemented'

type ScreenComponent = ComponentType

export const ROUTES: Record<string, ScreenComponent> = {
  'chat/list': ChatListScreen,
  'chat/detail': ChatDetailScreen,
  'projects/list': NotImplemented,
  'docs/list': NotImplemented,
  'settings/root': NotImplemented,
}
