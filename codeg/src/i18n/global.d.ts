import type enMessages from "@/i18n/messages/en.json"

declare module "next-intl" {
  interface AppConfig {
    // [crewchat] Narrowed to the two shipped locales (en / zh-CN).
    Locale: "en" | "zh-CN"
    Messages: typeof enMessages
  }
}
