# handlers to optimize

- [ ] `get_all_releases`
- [ ] `get_all_platforms_inner`
- [x] `rustdoc_redirector_handler`
- [x] `target_redirect_handler`
- [x] `rustdoc_html_server_handler`
- [ ] urls generated in templates?

## description / notes for PR

### goal

- remove duplication and differences in handling the path parameters in rustdoc
  handlers
- start generating redirect & canonical in the same param struct

idea is that for the "subdomain-per-crate" topic we then only need to change
this struct, so it can alternatively parse the subdomain, and generate the
correct redirect URLs.

### details

- parsing all parts from the request is put into the struct, so when we support
  separate subdomains for the crates, we just have to adapt this struct
- this means all url generation needs to be put into this param struct, or needs
  to be based on the struct to be able to differentiate between subdomain-access
  & main domain-access
- changes some generated URLs. instead of always pointing to `/index.html` on
  folders, we use the folder itself.
- also changes some target-redirect URLs, don't include the default target
