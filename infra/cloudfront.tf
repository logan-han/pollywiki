resource "aws_cloudfront_origin_access_control" "site" {
  name                              = "${var.project}-site"
  origin_access_control_origin_type = "s3"
  signing_behavior                  = "always"
  signing_protocol                  = "sigv4"
}

# Edge request handling: every page has exactly one URL, the apex host with a
# trailing slash, so www and slashless paths are 301s rather than second copies
# a crawler can index. Canonical /path/ then becomes /path/index.html because
# S3 REST origins do not serve directory indexes.
resource "aws_cloudfront_function" "index_rewrite" {
  name    = "${var.project}-index-rewrite"
  runtime = "cloudfront-js-2.0"
  publish = true
  code    = <<-EOT
    function handler(event) {
      var request = event.request;
      var host = request.headers.host && request.headers.host.value;
      var uri = request.uri;
      // Extension-less paths are page routes; files keep their own URL.
      var addSlash = !uri.endsWith('/') && !uri.includes('.');
      if (host === 'www.${var.domains[0]}' || addSlash) {
        // Carry the query string through; /search?q=x has to keep its term.
        var query = '';
        for (var name in request.querystring) {
          var param = request.querystring[name];
          var values = param.multiValue || [param];
          for (var i = 0; i < values.length; i++) {
            query += (query === '' ? '?' : '&') + name;
            if (values[i].value !== '') {
              query += '=' + values[i].value;
            }
          }
        }
        return {
          statusCode: 301,
          statusDescription: 'Moved Permanently',
          headers: {
            location: {
              value: 'https://${var.domains[0]}' + uri + (addSlash ? '/' : '') + query
            }
          }
        };
      }
      if (uri.endsWith('/')) {
        request.uri = uri + 'index.html';
      }
      return request;
    }
  EOT
}

data "aws_cloudfront_cache_policy" "optimized" {
  name = "Managed-CachingOptimized"
}

data "aws_cloudfront_response_headers_policy" "security" {
  name = "Managed-SecurityHeadersPolicy"
}

resource "aws_cloudfront_distribution" "site" {
  enabled             = true
  is_ipv6_enabled     = true
  comment             = var.project
  default_root_object = "index.html"
  http_version        = "http2and3"
  price_class         = "PriceClass_All"
  aliases             = var.enable_custom_domain ? var.domains : []

  origin {
    domain_name              = aws_s3_bucket.main.bucket_regional_domain_name
    origin_id                = "site-s3"
    origin_path              = "/site"
    origin_access_control_id = aws_cloudfront_origin_access_control.site.id
  }

  default_cache_behavior {
    target_origin_id           = "site-s3"
    viewer_protocol_policy     = "redirect-to-https"
    allowed_methods            = ["GET", "HEAD"]
    cached_methods             = ["GET", "HEAD"]
    compress                   = true
    cache_policy_id            = data.aws_cloudfront_cache_policy.optimized.id
    response_headers_policy_id = data.aws_cloudfront_response_headers_policy.security.id

    function_association {
      event_type   = "viewer-request"
      function_arn = aws_cloudfront_function.index_rewrite.arn
    }
  }

  # OAC returns 403 for missing keys; surface both as the 404 page.
  custom_error_response {
    error_code            = 403
    response_code         = 404
    response_page_path    = "/404.html"
    error_caching_min_ttl = 60
  }

  custom_error_response {
    error_code            = 404
    response_code         = 404
    response_page_path    = "/404.html"
    error_caching_min_ttl = 60
  }

  restrictions {
    geo_restriction {
      restriction_type = "none"
    }
  }

  viewer_certificate {
    cloudfront_default_certificate = var.enable_custom_domain ? null : true
    acm_certificate_arn            = var.enable_custom_domain ? local.certificate_arn : null
    ssl_support_method             = var.enable_custom_domain ? "sni-only" : null
    minimum_protocol_version       = var.enable_custom_domain ? "TLSv1.2_2021" : null
  }
}
