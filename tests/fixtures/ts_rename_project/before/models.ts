export class Account {
  ownerId?: number;

  load(ownerId: number): void {
    this.ownerId = ownerId;
  }
}
