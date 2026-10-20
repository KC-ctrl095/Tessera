// Commands shared by the normal tile and the sign-in drive-mode wrapper.
// A type-only import keeps the renderer out of the first-load bundle.
export interface WebViewControls {
  keyboard(): void;
  paste(): Promise<void>;
  desktopSite(): boolean;
  setDesktopSite(on: boolean): void;
  find(): void;
}
