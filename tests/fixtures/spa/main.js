// The portal's first page of notices is public.
export const notices = (page, size) => fetch(`api/notices/search?page=${page}&limit=${size}`);
export const advanced = body => fetch("/api/advanced/search", {method: "POST", body});
export const settings = () => fetch("/assets/settings.json");
