# KrateName everywhere

exceptions:

## `rustdoc_redirector_handler`
* sometimes gets filenames with dot, for static assets
* sometimes gets `krate::path_in_crate`

perhaps try parse? and keep the original element? But then declining has to
happen in match-version or similar?
