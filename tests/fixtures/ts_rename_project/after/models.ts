export class Account {
  ownerId?: string;

  load(ownerId: string): void {
    this.ownerId = ownerId;
  }
}
