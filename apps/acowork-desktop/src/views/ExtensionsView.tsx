/**
 * ExtensionsView — 扩展面板占位视图（VSCode Extensions 风格）。
 *
 * 当前为最小占位实现，提供一个清晰的入口让"扩展"按钮可点击导航；
 * 后续会把 Gateway 提供的扩展清单/启用/禁用接进来，详见
 * `core/acowork-gateway/src/http/extensions.rs`（计划中）。
 *
 * 外壳走全局胶囊布局（CAPSULE_PANE_CN，见 components/common/capsule.ts）：
 * 视图容器保持透明让窗口毛玻璃透出，中央这块占位自己画成一颗胶囊，
 * 与 pm / docs / 聊天视图的面板形状一致。
 */
import { CAPSULE_PANE_CN } from "../components/common/capsule";
import { cn } from "../lib/utils";

export function ExtensionsView() {
    return (
        // `w-full` matters for the same reason as in DocsView: as an
        // AppLayout flex item this root would otherwise size itself to its
        // content and leave bare vibrancy to the right of the capsule.
        <div className="flex h-full min-h-0 w-full flex-1">
            <div className={cn(CAPSULE_PANE_CN, "min-w-0 flex-1 bg-page-bg")}>
                <div className="flex flex-1 items-center justify-center">
                    <div className="flex flex-col items-center gap-3 text-center">
                        <div className="flex h-14 w-14 items-center justify-center rounded-xl border border-border-secondary text-text-tertiary">
                            <svg
                                viewBox="0 0 24 24"
                                fill="none"
                                stroke="currentColor"
                                strokeWidth="1.5"
                                strokeLinejoin="round"
                                className="h-7 w-7"
                                aria-hidden="true"
                            >
                                <rect x="3" y="3" width="8" height="8" rx="1.5" />
                                <rect x="13" y="3" width="8" height="6" rx="1.5" />
                                <rect x="3" y="13" width="6" height="8" rx="1.5" />
                                <rect x="11" y="11" width="10" height="10" rx="1.5" />
                            </svg>
                        </div>
                        <h2 className="text-base font-medium text-text-primary">Extensions</h2>
                        <p className="max-w-sm text-sm text-text-tertiary">
                            Extensions marketplace is coming soon.
                        </p>
                    </div>
                </div>
            </div>
        </div>
    );
}
