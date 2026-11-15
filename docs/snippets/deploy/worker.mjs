export class HarnessObject {
  constructor(ctx) {
    this.storage = ctx.storage;
  }

  async fetch() {
    return new Response("ok");
  }
}

export default {
  async fetch() {
    return new Response("not found", { status: 404 });
  },
};
