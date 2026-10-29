export const HISTORY_SCROLL_EDGE = 96;

export type HistoryPage = {
  hasMore: boolean;
  loading: boolean;
  load: () => Promise<void>;
};

/** One request per visit to the history edge, including after a failed page. */
export class HistoryPaging {
  private pending = false;
  private attempted = false;

  observe(top: number): void {
    if (top > HISTORY_SCROLL_EDGE) this.attempted = false;
  }

  async request(page: HistoryPage, automatic: boolean): Promise<void> {
    if (
      this.pending ||
      page.loading ||
      !page.hasMore ||
      (automatic && this.attempted)
    )
      return;
    this.attempted = true;
    this.pending = true;
    try {
      await page.load();
    } finally {
      this.pending = false;
    }
  }
}
