// 手机会话的沉浸状态独立于方向；自动旋转不会改变是否全屏。
export interface PhoneFullscreenRequest {
  stage: number;
  revision: number;
  fullscreen: boolean;
}

export class PhoneFullscreenPolicy {
  private fullscreen: boolean = false;
  private revision: number = 1;
  private stage: number = 0;
  private attached: boolean = false;
  private attemptedRevision: number = 0;
  private inFlight?: PhoneFullscreenRequest;

  observe(page: number, session: string): void {
    const next = page === 0 ? false : page === 3 && session.length > 0 ? true : this.fullscreen;
    if (next !== this.fullscreen) { this.fullscreen = next; this.revision++; }
  }

  attachWindow(): void {
    this.stage++;
    this.attached = true;
    this.attemptedRevision = 0;
    this.inFlight = undefined;
  }

  detachWindow(): void {
    this.stage++;
    this.attached = false;
    this.inFlight = undefined;
  }

  nextRequest(): PhoneFullscreenRequest | undefined {
    if (!this.attached || this.inFlight || this.attemptedRevision === this.revision) { return undefined; }
    const request: PhoneFullscreenRequest = { stage: this.stage, revision: this.revision, fullscreen: this.fullscreen };
    this.attemptedRevision = this.revision;
    this.inFlight = request;
    return request;
  }

  isCurrent(request: PhoneFullscreenRequest): boolean {
    return this.attached && request.stage === this.stage && request.revision === this.revision &&
      this.inFlight?.revision === request.revision;
  }

  complete(request: PhoneFullscreenRequest): boolean {
    if (!this.inFlight || request.stage !== this.stage || request.revision !== this.inFlight.revision) { return false; }
    this.inFlight = undefined;
    // 失败只记录一次，不让100ms快照轮询形成无限系统调用。
    return true;
  }
}
