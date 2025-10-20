# handlers to optimize

- [ ] `get_all_releases`
- [ ] `get_all_platforms_inner`
- [ ] `rustdoc_redirector_handler`
- [x] `target_redirect_handler`
- [ ] `rustdoc_html_server_handler`

## description / notes for PR

- parsing all parts from the request is put into the struct, so when we support
  separate subdomains for the crates, we just have to adapt this struct
- this means all url generation needs to be put into this param struct, or needs
  to be based on the struct to be able to differentiate between subdomain-access
  & main domain-access
