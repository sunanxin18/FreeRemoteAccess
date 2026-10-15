// 方向属于手机 shell；会话编号仅用于去重，不保存到日志或连接资料。
export const PHONE_SESSION_ORIENTATION_EVENT: string = 'frd_phone_session_orientation';

export enum PhoneOrientation { Portrait, Landscape }

export interface PhoneOrientationRequest {
  stage: number;
  revision: number;
  orientation: PhoneOrientation;
}

export class PhoneOrientationPolicy {
  private orientation: PhoneOrientation = PhoneOrientation.Portrait;
  private revision: number = 1;
  private login: boolean = true;
  private connectedSession: string = '';
  private stage: number = 0;
  private attached: boolean = false;
  private attemptedRevision: number = 0;
  private inFlight?: PhoneOrientationRequest;

  observe(page: number, session: string): void {
    if (page === 0) {
      if (!this.login) {
        this.orientation = PhoneOrientation.Portrait;
        this.revision++;
      }
      this.login = true;
      this.connectedSession = '';
    } else if (page === 3 && session.length > 0 && session !== this.connectedSession) {
      this.orientation = PhoneOrientation.Landscape;
      this.revision++;
      this.login = false;
      this.connectedSession = session;
    }
    // 连接、等待首帧、断开中和失败页均保留当前方向偏好。
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

  nextRequest(): PhoneOrientationRequest | undefined {
    if (!this.attached || this.inFlight || this.attemptedRevision === this.revision) { return undefined; }
    const request: PhoneOrientationRequest = { stage: this.stage, revision: this.revision, orientation: this.orientation };
    this.attemptedRevision = this.revision;
    this.inFlight = request;
    return request;
  }

  complete(request: PhoneOrientationRequest): boolean {
    if (!this.inFlight || request.stage !== this.stage || request.revision !== this.inFlight.revision) { return false; }
    this.inFlight = undefined;
    // 失败也消费此次请求；不因100ms轮询持续重试，下一次会话/窗口转换可重新申请。
    return true;
  }
}
