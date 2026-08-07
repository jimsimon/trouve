declare const __TROUVE_FRONTEND_VERSION__: string;
declare const __TROUVE_SLINT_VERSION__: string;
declare const __TROUVE_SOURCE_REVISION__: string;
declare const __TROUVE_PWA_CACHE_NAME__: string;

interface WindowEventMap {
  "trouve-pwa-update-ready": CustomEvent<{
    readonly activate: () => void;
  }>;
}
